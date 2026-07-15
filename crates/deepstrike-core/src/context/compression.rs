use super::config::ContextConfig;
use super::partitions::ContextPartitions;
use super::pressure::PressureAction;
use super::token_engine::ContextTokenEngine;
use super::units::{strict_tool_pairing_is_valid, unit_boundaries};
use super::utility::{UtilitySelectionContext, plan_utility_archive};
use crate::types::message::{Content, ContentPart, Message};

/// Compression result returned by every compactor.
#[derive(Default)]
pub struct CompressResult {
    /// Tokens freed from the partition.
    pub tokens_saved: u32,
    /// Generated summary text if any.
    pub summary: Option<String>,
    /// Messages drained/archived from the context.
    pub archived: Vec<Message>,
    /// Cache-aware (W1-1 step 2 / DoD #4): the earliest history-message index this op rewrote or
    /// removed — i.e. where it invalidates the prompt-cache prefix. `None` = prefix-safe (touched
    /// nothing). The pipeline folds the minimum across stages and surfaces it on the observation.
    pub prefix_invalidated_at: Option<usize>,
}

/// Compression strategy interface.
pub trait Compressor: Send + Sync {
    fn compress(
        &self,
        partitions: &mut ContextPartitions,
        target_tokens: u32,
        max_tokens: u32,
        preserve_k: usize,
        engine: &ContextTokenEngine,
    ) -> CompressResult;
}

/// rho > snip_threshold: cap each oversized message at `per_msg_tokens`.
pub struct SnipCompactor {
    pub per_msg_ratio: f64,
}

impl Compressor for SnipCompactor {
    fn compress(
        &self,
        partitions: &mut ContextPartitions,
        _target_tokens: u32,
        max_tokens: u32,
        preserve_k: usize,
        engine: &ContextTokenEngine,
    ) -> CompressResult {
        let per_msg_limit = ((max_tokens as f64 * self.per_msg_ratio) as u32).max(50);
        let mut saved = 0u32;
        let partition = &mut partitions.history;
        // Cache-prefix protection yields when there is no drop-fallback. An untouchable message —
        // protected-from-snip (idx < preserve_k) AND inside the drop floor (idx ≥ len − preserve_k*2)
        // — exists only when `len < preserve_k*3`. Below that threshold, disable protection so a
        // forced/413 compaction can always cap the oldest messages and free space; above it, the
        // prefix is droppable as a fallback, so we protect it (cache-aware).
        let prefix_keep = prefix_keep_for(partition.messages.len(), preserve_k);
        let indices =
            oversized_text_message_indices(&partition.messages, per_msg_limit, prefix_keep, engine);

        for &i in &indices {
            let msg = &mut partition.messages[i];
            let original_tokens = msg.token_count.unwrap_or_else(|| engine.count_message(msg));
            let head_limit = per_msg_limit / 2;
            let tail_limit = per_msg_limit.saturating_sub(head_limit);
            // Same head/tail elision as excerpt_text; the omitted count comes from the recorded
            // token metadata (not a recount) so the elision marker matches the saved accounting.
            let snipped = if let Content::Text(ref t) = msg.content {
                Some(excerpt_text_with_total(
                    t,
                    head_limit,
                    tail_limit,
                    engine,
                    original_tokens,
                ))
            } else {
                None
            };
            if let Some(text) = snipped {
                msg.content = Content::Text(text);
                msg.token_count = Some(per_msg_limit);
                saved += original_tokens.saturating_sub(per_msg_limit);
            }
        }

        partition.token_count = partition.token_count.saturating_sub(saved);

        // Pure executor: snip caps oversized messages in place; it never archives or summarizes.
        // Summary + compression-log attribution is the pipeline's job (under the *requested* action).
        CompressResult {
            tokens_saved: saved,
            prefix_invalidated_at: indices.iter().min().copied(),
            ..Default::default()
        }
    }
}

/// Pure selection (W1-1 collapse): indices of oversized **text** history messages a snip caps
/// (tokens > `per_msg_limit`; non-text and tiny ≤10-token messages skipped). The cache-aware planner
/// reuses this to choose which — and how far back — to snip; the executor only applies the head/tail
/// truncation to the chosen indices.
/// How many of the oldest messages to protect from in-place rewrites (snip/excerpt) as the stable
/// prompt-cache prefix. The protection **yields when there is no drop-fallback**: an untouchable
/// message (protected-from-snip `idx < preserve_k` AND inside the drop floor `idx ≥ len − preserve_k*2`)
/// exists only when `len < preserve_k*3`. Below that, return 0 so a forced/413 compaction can always
/// cap the oldest messages; at or above it, the prefix is droppable as a fallback, so protect it.
fn prefix_keep_for(len: usize, preserve_k: usize) -> usize {
    if len >= preserve_k.saturating_mul(3) {
        preserve_k
    } else {
        0
    }
}

fn oversized_text_message_indices(
    messages: &[Message],
    per_msg_limit: u32,
    prefix_keep: usize,
    engine: &ContextTokenEngine,
) -> Vec<usize> {
    messages
        .iter()
        .enumerate()
        .filter(|(i, msg)| {
            // Cache-aware (W1-1 step 2): never snip the oldest `prefix_keep` messages — they are the
            // stable prompt-cache prefix, and rewriting one invalidates the whole cache. Their tokens
            // are reclaimed by a batched DropOldest instead (which breaks the prefix exactly once).
            if *i < prefix_keep {
                return false;
            }
            if !matches!(msg.content, Content::Text(_)) {
                return false;
            }
            let toks = msg.token_count.unwrap_or_else(|| engine.count_message(msg));
            toks > per_msg_limit && toks > 10
        })
        .map(|(i, _)| i)
        .collect()
}

/// Helper to extract key fields and info from JSON strings.
fn extract_json_excerpt(output: &str) -> Option<String> {
    let val: serde_json::Value = serde_json::from_str(output).ok()?;
    match val {
        serde_json::Value::Object(map) => {
            let mut summary_parts = Vec::new();
            let mut keys = Vec::new();
            for (k, v) in &map {
                keys.push(k.as_str());
                if v.is_number() || v.is_boolean() {
                    summary_parts.push(format!("{}: {}", k, v));
                } else if let Some(s) = v.as_str() {
                    if s.len() <= 50 {
                        summary_parts.push(format!("{}: \"{}\"", k, s));
                    }
                }
            }
            Some(format!(
                "JSON Keys: [{}]\nJSON Fields: {{{}}}",
                keys.join(", "),
                summary_parts.join(", ")
            ))
        }
        serde_json::Value::Array(arr) => {
            if arr.is_empty() {
                return Some("JSON Array: []".to_string());
            }
            let mut headers = Vec::new();
            if let Some(serde_json::Value::Object(first_map)) = arr.first() {
                for k in first_map.keys() {
                    headers.push(k.as_str());
                }
            }
            let len = arr.len();
            Some(format!(
                "JSON Array: {} items. Keys: [{}]",
                len,
                headers.join(", ")
            ))
        }
        _ => None,
    }
}

/// Helper to keep a specific amount of head and tail tokens.
fn excerpt_text(
    text: &str,
    head_tokens: u32,
    tail_tokens: u32,
    engine: &ContextTokenEngine,
) -> String {
    excerpt_text_with_total(text, head_tokens, tail_tokens, engine, engine.count(text))
}

/// [`excerpt_text`] with the total token count supplied by the caller (e.g. from recorded
/// message metadata) instead of recounted — the count only feeds the elision marker.
fn excerpt_text_with_total(
    text: &str,
    head_tokens: u32,
    tail_tokens: u32,
    engine: &ContextTokenEngine,
    total_tokens: u32,
) -> String {
    if total_tokens <= head_tokens + tail_tokens {
        return text.to_string();
    }
    let head = engine.truncate(text, head_tokens);

    let chars: Vec<char> = text.chars().collect();
    let mut low = head.chars().count();
    let mut high = chars.len();
    let mut suffix_start = chars.len();
    while low <= high {
        let mid = (low + high) / 2;
        if mid >= chars.len() {
            break;
        }
        let candidate: String = chars[mid..].iter().collect();
        let tokens = engine.count(&candidate);
        if tokens <= tail_tokens {
            suffix_start = mid;
            if mid == 0 {
                break;
            }
            high = mid - 1;
        } else {
            low = mid + 1;
        }
    }
    let tail: String = chars[suffix_start..].iter().collect();
    let remaining = total_tokens
        .saturating_sub(head_tokens)
        .saturating_sub(tail_tokens);
    format!("{}… [… {} tokens omitted …] …{}", head, remaining, tail)
}

/// Pure selection (W1-1 collapse): indices of history messages whose large (≥200-token) content
/// contains at least one tool result not named in `preserved_refs`. The executor excerpts every
/// eligible result in the selected envelope. The cache-aware planner reuses this: tool
/// results are interleaved mid/late history, so excerpting them is prefix-safe.
fn excerptable_tool_result_indices(
    messages: &[Message],
    preserved_refs: &[String],
    prefix_keep: usize,
    engine: &ContextTokenEngine,
) -> Vec<usize> {
    messages
        .iter()
        .enumerate()
        .filter_map(|(i, msg)| {
            // Cache-aware (W1-1 step 2): protect the oldest `prefix_keep` messages from in-place
            // excerpting (they are the stable prompt-cache prefix).
            if i < prefix_keep {
                return None;
            }
            let toks = msg.token_count.unwrap_or_else(|| engine.count_message(msg));
            if toks < 200 {
                return None;
            }
            let Content::Parts(parts) = &msg.content else {
                return None;
            };
            parts
                .iter()
                .any(|part| {
                    matches!(
                        part,
                        ContentPart::ToolResult { call_id, .. }
                            if !preserved_refs.iter().any(|preserved| preserved == call_id.as_str())
                    )
                })
                .then_some(i)
        })
        .collect()
}

/// rho > micro_threshold: replace tool results with a compact excerpt. Selection via
/// [`excerptable_tool_result_indices`]; this executor only applies the excerpt.
pub struct MicroCompactor;

impl Compressor for MicroCompactor {
    fn compress(
        &self,
        partitions: &mut ContextPartitions,
        _target_tokens: u32,
        _max_tokens: u32,
        preserve_k: usize,
        engine: &ContextTokenEngine,
    ) -> CompressResult {
        let find_tool_name = |call_id: &str, msgs: &[Message]| -> Option<String> {
            for m in msgs {
                for tc in &m.tool_calls {
                    if tc.id == call_id {
                        return Some(tc.name.to_string());
                    }
                }
            }
            None
        };

        // Selection lifted to a pure helper (excludes `preserved_refs` + the cache-prefix when it has
        // a drop-fallback); the executor only applies the excerpt to the chosen tool-result messages.
        let prefix_keep = prefix_keep_for(partitions.history.messages.len(), preserve_k);
        let indices = excerptable_tool_result_indices(
            &partitions.history.messages,
            &partitions.task_state.preserved_refs,
            prefix_keep,
            engine,
        );
        let messages_clone = partitions.history.messages.clone();
        let preserved_refs = partitions.task_state.preserved_refs.clone();
        let partition = &mut partitions.history;
        let mut saved = 0u32;

        for &i in &indices {
            let msg = &mut partition.messages[i];
            let original_tokens = msg.token_count.unwrap_or_else(|| engine.count_message(msg));
            if let Content::Parts(ref mut parts) = msg.content {
                for part in parts.iter_mut() {
                    if let ContentPart::ToolResult {
                        call_id,
                        output,
                        is_error: _,
                    } = part
                    {
                        if preserved_refs
                            .iter()
                            .any(|preserved| preserved == call_id.as_str())
                        {
                            continue;
                        }
                        let original_output_tokens = engine.count(output);
                        let tool_name = find_tool_name(call_id, &messages_clone)
                            .unwrap_or_else(|| "unknown".to_string());

                        let new_output = if original_output_tokens > 2000 {
                            if let Some(json_excerpt) = extract_json_excerpt(output) {
                                format!(
                                    "[tool result: {} | {} | {} tokens]\n{}",
                                    call_id, tool_name, original_output_tokens, json_excerpt
                                )
                            } else {
                                let excerpt = excerpt_text(output, 30, 10, engine);
                                format!(
                                    "[tool result: {} | {} | {} tokens]\n{}",
                                    call_id, tool_name, original_output_tokens, excerpt
                                )
                            }
                        } else {
                            let excerpt = excerpt_text(output, 150, 50, engine);
                            format!(
                                "[tool result: {} | {} | {} tokens]\n{}",
                                call_id, tool_name, original_output_tokens, excerpt
                            )
                        };

                        *output = new_output;
                    }
                }
                let new_tokens = engine.count_message(msg);
                msg.token_count = Some(new_tokens);
                saved += original_tokens.saturating_sub(new_tokens);
            }
        }

        partition.token_count = partition.token_count.saturating_sub(saved);

        // Pure executor: excerpts tool results in place; no archive, summary, or self-log.
        CompressResult {
            tokens_saved: saved,
            prefix_invalidated_at: indices.iter().min().copied(),
            ..Default::default()
        }
    }
}

/// Pure **selection** (W1-1 collapse): how many of the oldest history messages to drop to bring the
/// partition under `target_tokens`, never splitting a context unit or crossing the
/// preserve-recent floor (`keep` units).
/// Returns `(count, tokens_saved)`; the executor just drains `count` from the front. This is the
/// decision the cache-aware planner reuses to "batch one big drop to target" rather than re-deriving
/// the count inside the compactor.
pub fn plan_drop_oldest(
    messages: &[Message],
    total_tokens: u32,
    target_tokens: u32,
    keep: usize,
    engine: &ContextTokenEngine,
) -> (usize, u32) {
    let units = unit_boundaries(messages);
    let limit = units.len().saturating_sub(keep);
    let mut saved = 0u32;
    let mut n = 0usize;
    for unit in units.iter().take(limit) {
        if total_tokens.saturating_sub(saved) <= target_tokens {
            break;
        }
        saved += messages[unit.clone()]
            .iter()
            .map(|msg| msg.token_count.unwrap_or_else(|| engine.count_message(msg)))
            .sum::<u32>();
        n = unit.end;
    }
    (n, saved)
}

/// rho > collapse_threshold: drop oldest messages until within target. Selection via
/// [`plan_drop_oldest`]; this executor only drains the chosen count.
pub struct CollapseCompactor;

impl Compressor for CollapseCompactor {
    fn compress(
        &self,
        partitions: &mut ContextPartitions,
        target_tokens: u32,
        _max_tokens: u32,
        preserve_k: usize,
        engine: &ContextTokenEngine,
    ) -> CompressResult {
        let non_history_tokens = partitions
            .total_tokens(engine)
            .saturating_sub(partitions.history.token_count);
        let history_target = target_tokens.saturating_sub(non_history_tokens);
        let plan = plan_utility_archive(
            &partitions.history.messages,
            partitions.history.token_count,
            history_target,
            preserve_k,
            engine,
            &UtilitySelectionContext {
                goal: &partitions.task_state.goal,
                criteria: &partitions.task_state.criteria,
                preserved_refs: &partitions.task_state.preserved_refs,
                active_directives: &partitions.task_state.directives,
            },
        );
        if plan.archived_ranges.is_empty() {
            return CompressResult::default();
        }
        let prefix_invalidated_at = plan.archived_ranges.iter().map(|range| range.start).min();
        let (archived, saved) = apply_utility_plan(&mut partitions.history, &plan);

        // Pure executor: return the drained messages; the pipeline summarizes + logs once under the
        // requested action. Removing an interior unit invalidates from its original start index.
        CompressResult {
            tokens_saved: saved,
            archived,
            prefix_invalidated_at,
            ..Default::default()
        }
    }
}

/// rho > auto_threshold: collapse history entirely except last K turns, updating compression log.
pub struct AutoCompactor;

impl Compressor for AutoCompactor {
    fn compress(
        &self,
        partitions: &mut ContextPartitions,
        target_tokens: u32,
        _max_tokens: u32,
        preserve_k: usize,
        engine: &ContextTokenEngine,
    ) -> CompressResult {
        if partitions.history.messages.is_empty() {
            return CompressResult::default();
        }
        let non_history_tokens = partitions
            .total_tokens(engine)
            .saturating_sub(partitions.history.token_count);
        let history_target = target_tokens.saturating_sub(non_history_tokens);
        let plan = plan_utility_archive(
            &partitions.history.messages,
            partitions.history.token_count,
            history_target,
            preserve_k,
            engine,
            &UtilitySelectionContext {
                goal: &partitions.task_state.goal,
                criteria: &partitions.task_state.criteria,
                preserved_refs: &partitions.task_state.preserved_refs,
                active_directives: &partitions.task_state.directives,
            },
        );
        if plan.archived_ranges.is_empty() {
            return CompressResult::default();
        }
        let prefix_invalidated_at = plan.archived_ranges.iter().map(|range| range.start).min();
        let (archived, saved) = apply_utility_plan(&mut partitions.history, &plan);

        // Pure executor: return the drained messages; the pipeline summarizes + logs once under the
        // requested action.
        CompressResult {
            tokens_saved: saved,
            archived,
            prefix_invalidated_at,
            ..Default::default()
        }
    }
}

fn apply_utility_plan(
    partition: &mut super::partitions::Partition,
    plan: &super::utility::UtilityArchivePlan,
) -> (Vec<Message>, u32) {
    let pairing_was_valid = strict_tool_pairing_is_valid(&partition.messages);
    let archived_indices = plan
        .archived_ranges
        .iter()
        .flat_map(|range| range.clone())
        .collect::<std::collections::BTreeSet<_>>();
    let mut archived = Vec::new();
    let mut retained = Vec::new();
    for (index, message) in std::mem::take(&mut partition.messages)
        .into_iter()
        .enumerate()
    {
        if archived_indices.contains(&index) {
            archived.push(message);
        } else {
            retained.push(message);
        }
    }
    partition.messages = retained;
    partition.token_count = plan.retained_tokens;
    debug_assert!(
        !pairing_was_valid || strict_tool_pairing_is_valid(&partition.messages),
        "utility selection split a valid tool transaction"
    );
    (archived, plan.archived_tokens)
}

/// Compression pipeline — operates on history partition but can reference full partitions.
pub struct CompressionPipeline {
    stages: Vec<(PressureAction, Box<dyn Compressor>)>,
    preserve_recent_turns: usize,
}

impl CompressionPipeline {
    pub fn new(config: &ContextConfig) -> Self {
        Self {
            preserve_recent_turns: config.preserve_recent_turns,
            stages: vec![
                (
                    PressureAction::SnipCompact,
                    Box::new(SnipCompactor {
                        per_msg_ratio: config.snip_per_msg_ratio,
                    }),
                ),
                (PressureAction::MicroCompact, Box::new(MicroCompactor)),
                (PressureAction::ContextCollapse, Box::new(CollapseCompactor)),
                (PressureAction::AutoCompact, Box::new(AutoCompactor)),
            ],
        }
    }

    pub fn compress(
        &self,
        partitions: &mut ContextPartitions,
        action: PressureAction,
        max_tokens: u32,
        target_tokens: u32,
        engine: &ContextTokenEngine,
    ) -> (u32, Option<String>, Vec<Message>, Option<usize>) {
        if action == PressureAction::None {
            return (0, None, vec![], None);
        }

        let mut total_saved = 0;
        let mut all_archived = vec![];
        // Cache cost of the whole compaction = the earliest prefix-break across the stages that ran
        // (an earlier break dominates). `None` = entirely prefix-safe.
        let mut cache_at: Option<usize> = None;
        let summarizer = super::summarizer::RuleSummarizer;

        for (stage_action, compressor) in &self.stages {
            if *stage_action <= action {
                if partitions.total_tokens(engine) <= target_tokens {
                    break;
                }
                let res = compressor.compress(
                    partitions,
                    target_tokens,
                    max_tokens,
                    self.preserve_recent_turns,
                    engine,
                );
                total_saved += res.tokens_saved;
                cache_at = [cache_at, res.prefix_invalidated_at]
                    .into_iter()
                    .flatten()
                    .min();
                all_archived.extend(res.archived);
            }
        }

        // Single decision point for summary + log attribution: whatever the cascade drained is
        // summarized ONCE under the **requested** action and logged once. The compactors are pure
        // executors that no longer self-attribute — so a `compress(AutoCompact)` whose draining
        // happened in the Collapse stage is still labeled `auto_compact` (the C fix), and a
        // `compress(ContextCollapse)` stays `context_collapse` (unchanged).
        // The summary budget is the room the summary may occupy in the *compacted* window — a
        // separate concern from `target_tokens` (how small history must get). They coincide for
        // Collapse (non-zero target), but Auto-Compact drives history toward 0, so reusing the
        // target here would budget the summary at 0 tokens and emit an empty summary for the very
        // tier whose whole purpose is to replace archived history with a compact record. Fall back
        // to the full context window when the target is 0; the summariser self-bounds by structure.
        let summary_budget = if target_tokens == 0 {
            max_tokens
        } else {
            target_tokens
        };
        let summary = if all_archived.is_empty() {
            None
        } else {
            let s = summarizer.summarize(&all_archived, action, summary_budget);
            partitions
                .task_state
                .log_compression(action.label(), s.clone());
            Some(s)
        };

        (total_saved, summary, all_archived, cache_at)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::config::ContextConfig;
    use crate::context::partitions::ContextPartitions;
    use crate::context::token_engine::ContextTokenEngine;
    use crate::types::message::Message;

    fn engine() -> ContextTokenEngine {
        ContextTokenEngine::char_approx()
    }
    fn config() -> ContextConfig {
        ContextConfig::default()
    }
    const MAX: u32 = 1_000;

    #[test]
    fn snip_compactor_truncates_oversized_messages() {
        let cfg = ContextConfig {
            snip_per_msg_ratio: 0.10,
            ..Default::default()
        };
        let compactor = SnipCompactor {
            per_msg_ratio: cfg.snip_per_msg_ratio,
        };
        let mut ctx = ContextPartitions::new(&cfg);
        ctx.history.push(Message::user("a".repeat(800)), 200);
        // preserve_k=0: exercise the truncation transform directly (no cache-prefix protection).
        let result = compactor.compress(&mut ctx, 0, MAX, 0, &engine());
        assert!(result.tokens_saved > 0);
        if let Content::Text(ref t) = ctx.history.messages[0].content {
            assert!(t.contains("… [… 100 tokens omitted …] …"), "got: {t}");
        }
    }

    #[test]
    fn snip_compactor_leaves_small_messages_untouched() {
        let cfg = ContextConfig {
            snip_per_msg_ratio: 0.10,
            ..Default::default()
        };
        let compactor = SnipCompactor {
            per_msg_ratio: cfg.snip_per_msg_ratio,
        };
        let mut ctx = ContextPartitions::new(&cfg);
        ctx.history.push(Message::user("short"), 5);
        let result = compactor.compress(&mut ctx, 0, MAX, 2, &engine());
        assert_eq!(result.tokens_saved, 0);
    }

    #[test]
    fn micro_compactor_replaces_tool_results_with_measured_placeholder() {
        use crate::types::message::{ContentPart, Role};
        use compact_str::CompactString;

        let compactor = MicroCompactor;
        let mut ctx = ContextPartitions::new(&config());
        let parts = vec![ContentPart::ToolResult {
            call_id: CompactString::new("c1"),
            output: "a".repeat(1200),
            is_error: false,
        }];
        let msg = Message {
            role: Role::Tool,
            content: Content::Parts(parts),
            tool_calls: vec![],
            token_count: Some(300),
        };
        ctx.history.messages.push(msg);
        ctx.history.token_count = 300;

        // preserve_k=0: exercise the excerpt transform directly (no cache-prefix protection).
        let result = compactor.compress(&mut ctx, 0, MAX, 0, &engine());
        assert!(result.tokens_saved > 0);
        let Content::Parts(parts) = &ctx.history.messages[0].content else {
            panic!("tool-result envelope must survive compaction");
        };
        let text = parts
            .iter()
            .find_map(|part| match part {
                ContentPart::ToolResult {
                    call_id, output, ..
                } if call_id.as_str() == "c1" => Some(output.as_str()),
                _ => None,
            })
            .expect("correlated tool result remains present");
        assert!(
            text.contains("[tool result: c1 | unknown | 300 tokens]"),
            "got: {text}"
        );
    }

    #[test]
    fn micro_compactor_recounts_the_entire_mixed_parts_envelope() {
        use crate::types::message::{ContentPart, Role};

        let compactor = MicroCompactor;
        let mut ctx = ContextPartitions::new(&config());
        let msg = Message {
            role: Role::Tool,
            content: Content::Parts(vec![
                ContentPart::Text {
                    text: "metadata that must remain budgeted".into(),
                },
                ContentPart::ToolResult {
                    call_id: "c1".into(),
                    output: "a".repeat(1200),
                    is_error: false,
                },
                ContentPart::ToolResult {
                    call_id: "c2".into(),
                    output: "b".repeat(1000),
                    is_error: false,
                },
            ]),
            tool_calls: vec![],
            token_count: None,
        };
        let original = engine().count_message(&msg);
        ctx.history.push(msg, original);

        compactor.compress(&mut ctx, 0, MAX, 0, &engine());

        let message = &ctx.history.messages[0];
        let recounted = engine().count_message(message);
        assert_eq!(message.token_count, Some(recounted));
        assert_eq!(ctx.history.token_count, recounted);
        let Content::Parts(parts) = &message.content else {
            panic!("parts preserved")
        };
        assert_eq!(
            parts
                .iter()
                .filter(|part| matches!(part, ContentPart::ToolResult { output, .. } if output.starts_with("[tool result:")))
                .count(),
            2,
            "every eligible tool result is excerpted"
        );
    }

    #[test]
    fn collapse_compactor_drops_oldest_to_reach_target() {
        let compactor = CollapseCompactor;
        let mut ctx = ContextPartitions::new(&config());
        for _ in 0..8 {
            ctx.history.push(Message::user("msg"), 50);
        }
        let result = compactor.compress(&mut ctx, 250, MAX, 2, &engine());
        assert!(result.tokens_saved > 0);
        assert!(ctx.history.messages.len() < 8);
        // Pure executor: returns the drained messages; summary + log attribution is the pipeline's
        // job (under the requested action), so the compactor itself no longer summarizes or logs.
        assert!(
            !result.archived.is_empty(),
            "drained messages are returned to the pipeline"
        );
        assert!(
            result.summary.is_none(),
            "compactor no longer self-summarizes"
        );
        assert!(
            ctx.task_state.compression_log.is_empty(),
            "compactor no longer logs"
        );
    }

    #[test]
    fn collapse_utility_selection_changes_the_archived_units_not_only_the_score() {
        let compactor = CollapseCompactor;
        let mut ctx = ContextPartitions::new(&config());
        ctx.task_state.goal = "ship ORCHID release".into();
        for (user, assistant) in [
            (
                "ORCHID release criterion",
                "DECISION: retry failure; artifact /work/orchid.json",
            ),
            ("routine chatter one", "acknowledged"),
            ("routine chatter two", "acknowledged"),
            ("latest request", "working on it"),
        ] {
            ctx.history.push(Message::user(user), 40);
            ctx.history.push(Message::assistant(assistant), 40);
        }

        let result = compactor.compress(&mut ctx, 160, MAX, 1, &engine());
        let retained = ctx
            .history
            .messages
            .iter()
            .filter_map(|message| message.content.as_text())
            .collect::<Vec<_>>()
            .join("\n");
        let archived = result
            .archived
            .iter()
            .filter_map(|message| message.content.as_text())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(retained.contains("ORCHID"));
        assert!(retained.contains("/work/orchid.json"));
        assert!(!retained.contains("routine chatter"));
        assert!(archived.contains("routine chatter one"));
        assert!(archived.contains("routine chatter two"));
        assert_eq!(ctx.history.token_count, 160);
    }

    #[test]
    fn utility_selector_deducts_fixed_context_before_budgeting_history() {
        let compactor = CollapseCompactor;
        let mut ctx = ContextPartitions::new(&config());
        ctx.system.push(Message::system("fixed"), 600);
        for index in 0..4 {
            ctx.history
                .push(Message::user(format!("unit {index}")), 100);
        }

        let result = compactor.compress(&mut ctx, 700, MAX, 1, &engine());
        assert_eq!(result.tokens_saved, 300);
        assert_eq!(ctx.history.token_count, 100);
        assert!(ctx.total_tokens(&engine()) <= 700);
    }

    #[test]
    fn rule_summarizer_formats_correctly() {
        use crate::context::summarizer::RuleSummarizer;
        use crate::types::message::{Content, Message, Role};
        let summarizer = RuleSummarizer;
        let mut messages = vec![];
        messages.push(Message {
            role: Role::User,
            content: Content::Text("hello".to_string()),
            tool_calls: vec![],
            token_count: Some(5),
        });
        messages.push(Message {
            role: Role::Assistant,
            content: Content::Text("world".to_string()),
            tool_calls: vec![],
            token_count: Some(6),
        });
        let summary = summarizer.summarize(&messages, PressureAction::SnipCompact, 100);
        assert!(summary.contains("[Compressed: snip_compact]"));
        assert!(summary.contains("archived_messages: 2; archived_tokens: 11"));
        assert!(summary.contains("constraints:"));
    }

    #[test]
    fn micro_compactor_preserves_refs_in_preserved_refs() {
        use crate::types::message::{ContentPart, Role};
        use compact_str::CompactString;

        let compactor = MicroCompactor;
        let mut ctx = ContextPartitions::new(&config());
        ctx.task_state.preserved_refs = vec!["keep_me".to_string()];

        let parts = vec![ContentPart::ToolResult {
            call_id: CompactString::new("keep_me"),
            output: "a".repeat(1200),
            is_error: false,
        }];
        let msg = Message {
            role: Role::Tool,
            content: Content::Parts(parts),
            tool_calls: vec![],
            token_count: Some(300),
        };
        ctx.history.messages.push(msg);
        ctx.history.token_count = 300;

        let result = compactor.compress(&mut ctx, 0, MAX, 2, &engine());
        // Since call_id "keep_me" is in preserved_refs, it should not be replaced!
        assert_eq!(result.tokens_saved, 0);
        let text_opt = ctx.history.messages[0].content.as_text();
        assert!(
            text_opt.is_none(),
            "should not be replaced to text placeholder"
        );
    }

    #[test]
    fn auto_compactor_merges_all_except_last_two_turns() {
        let compactor = AutoCompactor;
        let mut ctx = ContextPartitions::new(&config());
        for i in 0..10 {
            ctx.history.push(Message::user(format!("msg {i}")), 10);
        }
        let result = compactor.compress(&mut ctx, 0, MAX, 2, &engine());
        assert!(result.tokens_saved > 0);
        assert_eq!(ctx.history.messages.len(), 2); // kept last 2 semantic units
        // Pure executor: returns the drained messages; the pipeline summarizes + logs under the
        // requested action (see `baseline_auto_*` / `pipeline_attributes_summary_to_requested_action`).
        assert!(
            !result.archived.is_empty(),
            "drained messages returned to the pipeline"
        );
        assert!(
            result.summary.is_none(),
            "compactor no longer self-summarizes"
        );
        assert!(
            ctx.task_state.compression_log.is_empty(),
            "compactor no longer logs"
        );
    }

    #[test]
    fn plan_drop_oldest_respects_target_and_preserve_floor() {
        // Pure selection helper (W1-1 collapse): drop the fewest oldest messages to reach target,
        // never below the preserve floor. This is the decision the cache-aware planner reuses.
        let msgs: Vec<Message> = (0..8)
            .map(|i| {
                let mut m = Message::user(format!("m{i}"));
                m.token_count = Some(50);
                m
            })
            .collect();
        // total=400, target=250, keep=2 → drop 3 oldest (150 saved) lands exactly at 250.
        assert_eq!(plan_drop_oldest(&msgs, 400, 250, 2, &engine()), (3, 150));
        // target=0 with keep=2 → drains down to the floor (len-keep = 6), never below it.
        assert_eq!(plan_drop_oldest(&msgs, 400, 0, 2, &engine()), (6, 300));
        // already under target → no drop.
        assert_eq!(plan_drop_oldest(&msgs, 400, 500, 2, &engine()), (0, 0));
    }

    #[test]
    fn collapse_never_splits_a_tool_transaction() {
        let mut call = Message::assistant("calling");
        call.tool_calls.push(crate::types::message::ToolCall {
            id: "call-1".into(),
            name: "read".into(),
            arguments: serde_json::json!({}),
        });
        let messages = vec![
            Message::user("question"),
            call,
            Message::tool(vec![ContentPart::ToolResult {
                call_id: "call-1".into(),
                output: "ok".into(),
                is_error: false,
            }]),
            Message::assistant("answer"),
            Message::user("next"),
            Message::assistant("done"),
        ]
        .into_iter()
        .map(|mut message| {
            message.token_count = Some(10);
            message
        })
        .collect::<Vec<_>>();

        assert_eq!(plan_drop_oldest(&messages, 60, 30, 1, &engine()), (4, 40));
    }

    #[test]
    fn auto_compactor_preserves_the_latest_complete_tool_unit() {
        let compactor = AutoCompactor;
        let mut ctx = ContextPartitions::new(&config());
        ctx.history.push(Message::user("old"), 10);
        ctx.history.push(Message::assistant("old answer"), 10);
        let mut call = Message::assistant("calling");
        call.tool_calls.push(crate::types::message::ToolCall {
            id: "call-1".into(),
            name: "read".into(),
            arguments: serde_json::json!({}),
        });
        ctx.history.push(Message::user("question"), 10);
        ctx.history.push(call, 10);
        ctx.history.push(
            Message::tool(vec![ContentPart::ToolResult {
                call_id: "call-1".into(),
                output: "ok".into(),
                is_error: false,
            }]),
            10,
        );
        ctx.history.push(Message::assistant("answer"), 10);

        compactor.compress(&mut ctx, 0, MAX, 1, &engine());

        assert_eq!(ctx.history.messages.len(), 4);
        assert_eq!(ctx.history.messages[0].content.as_text(), Some("question"));
    }

    #[test]
    fn prefix_keep_yields_without_drop_fallback() {
        // Protect the oldest `preserve_k` only when the history is large enough that they remain
        // droppable (len >= preserve_k*3); otherwise 0, so a forced/413 compaction can cap them.
        assert_eq!(prefix_keep_for(6, 2), 2, "len 6 >= 6 → protect oldest 2");
        assert_eq!(
            prefix_keep_for(5, 2),
            0,
            "len 5 < 6 → would leave an untouchable message"
        );
        assert_eq!(prefix_keep_for(3, 2), 0);
        assert_eq!(prefix_keep_for(0, 2), 0);
    }

    #[test]
    fn pipeline_reports_accurate_prefix_invalidation() {
        // (a) DoD #4: the pipeline surfaces the earliest message any stage actually touched. On the
        // len=6 baseline (prefix_keep=2), a SnipCompact protects the oldest 2 and caps msgs 3,4 — so
        // the cache break is at index 3, NOT the coarse 0. An AutoCompact drops the oldest → break 0.
        let mut cfg = config();
        cfg.preserve_recent_turns = 1;
        let mut ctx = baseline_partitions();
        let (_s, _u, _a, cache_at) = CompressionPipeline::new(&cfg).compress(
            &mut ctx,
            PressureAction::SnipCompact,
            MAX,
            500,
            &engine(),
        );
        assert_eq!(
            cache_at,
            Some(1),
            "one protected unit leaves msg 1 as the earliest rewrite"
        );

        let mut ctx2 = baseline_partitions();
        let (_s2, _u2, _a2, cache_at2) = CompressionPipeline::new(&cfg).compress(
            &mut ctx2,
            PressureAction::AutoCompact,
            MAX,
            500,
            &engine(),
        );
        assert_eq!(
            cache_at2,
            Some(0),
            "dropping the oldest breaks the cache prefix at 0"
        );
    }

    // ─── W1-1 characterization baseline ────────────────────────────────────────
    // Locks the CURRENT compaction behavior (tokens_saved / archived count / summary)
    // across all four pressure levels + the cascade, so the upcoming compactor→executor
    // refactor (EvictionOp vocab + cache-aware planner) is provably behavior-preserving.
    // These are golden-master pins: the values describe what the pipeline does TODAY, not
    // an independent derivation. If a future change moves a number here, that is a behavior
    // change and must be justified, not blindly re-pinned.

    use crate::types::message::Role;
    use compact_str::CompactString;

    /// Deterministic fixture: 4 oversized text turns + 2 tool-result messages, explicit token
    /// counts so the cascade math is reproducible under `char_approx`.
    fn baseline_partitions() -> ContextPartitions {
        let cfg = config();
        let mut ctx = ContextPartitions::new(&cfg);
        // Oversized text turns (trigger Snip / Collapse / Auto).
        ctx.history.push(Message::user("u0 ".repeat(120)), 300);
        ctx.history.push(Message::assistant("a0 ".repeat(120)), 300);
        // Tool-result message (trigger Micro).
        ctx.history.messages.push(Message {
            role: Role::Tool,
            content: Content::Parts(vec![ContentPart::ToolResult {
                call_id: CompactString::new("call_1"),
                output: serde_json::json!({"rows": 42, "ok": true, "name": "alpha"}).to_string()
                    + &"-pad".repeat(400),
                is_error: false,
            }]),
            tool_calls: vec![],
            token_count: Some(400),
        });
        ctx.history.token_count += 400;
        ctx.history.push(Message::user("u1 ".repeat(120)), 300);
        ctx.history.push(Message::assistant("a1 ".repeat(120)), 300);
        ctx.history.messages.push(Message {
            role: Role::Tool,
            content: Content::Parts(vec![ContentPart::ToolResult {
                call_id: CompactString::new("call_2"),
                output: "y".repeat(1600),
                is_error: false,
            }]),
            tool_calls: vec![],
            token_count: Some(400),
        });
        ctx.history.token_count += 400;
        ctx
    }

    /// Run the pipeline on a fresh baseline fixture at one action level.
    /// Returns `(before, saved, summary, archived_len, msgs_after, total_after)`.
    fn run_baseline(action: PressureAction) -> (u32, u32, Option<String>, usize, usize, u32) {
        let mut ctx = baseline_partitions();
        let before = ctx.total_tokens(&engine());
        let mut cfg = config();
        cfg.preserve_recent_turns = 1;
        let (saved, summary, archived, _cache_at) =
            CompressionPipeline::new(&cfg).compress(&mut ctx, action, MAX, 500, &engine());
        let archived_len = archived.len();
        let msgs_after = ctx.history.messages.len();
        let total_after = ctx.total_tokens(&engine());
        (
            before,
            saved,
            summary,
            archived_len,
            msgs_after,
            total_after,
        )
    }

    #[test]
    fn baseline_snip_only_caps_text_no_archival() {
        // SnipCompact runs only the Snip stage: caps oversized text messages in place — EXCEPT the
        // With one recent semantic unit protected by the fixture policy, the cache-prefix rule
        // protects only msg 0 from in-place rewriting. Three oversized text messages are capped.
        let (before, saved, summary, archived, msgs, total) =
            run_baseline(PressureAction::SnipCompact);
        assert_eq!(before, 2000);
        assert_eq!(saved, 750, "3 non-prefix oversized messages × 250");
        assert_eq!(archived, 0);
        assert!(summary.is_none());
        assert_eq!(msgs, 6, "snip mutates in place, drops no messages");
        assert_eq!(total, 1250);
    }

    #[test]
    fn baseline_micro_excerpts_tool_results() {
        // MicroCompact runs Snip then Micro under the same one-unit protection policy.
        let (before, saved, summary, archived, msgs, total) =
            run_baseline(PressureAction::MicroCompact);
        assert_eq!(before, 2000);
        assert_eq!(saved, 1112, "snip(750) + tool-result excerpts(362)");
        assert_eq!(archived, 0);
        assert!(summary.is_none());
        assert_eq!(msgs, 6);
        assert_eq!(total, 888);
    }

    #[test]
    fn baseline_collapse_drops_oldest_and_summarizes() {
        // ContextCollapse runs Snip→Micro→Collapse: oldest messages drained to `archived` with a
        // summary, down to the preserve-recent floor (one complete unit kept).
        let (before, saved, summary, archived, msgs, total) =
            run_baseline(PressureAction::ContextCollapse);
        assert_eq!(before, 2000);
        assert_eq!(saved, 1681);
        assert_eq!(archived, 3, "drops the complete oldest unit");
        assert_eq!(msgs, 3, "one complete recent unit remains");
        assert_eq!(total, 319);
        let summary = summary.expect("collapse summarizes archived messages");
        assert!(
            summary.contains("[Compressed: context_collapse]"),
            "summary routes the collapse action: {summary}"
        );
    }

    #[test]
    fn baseline_auto_attributes_summary_to_auto_compact() {
        // AutoCompact runs all 4 stages; on this fixture Snip→Micro→Collapse already hit the preserve
        // floor, so the Auto *stage* archives nothing extra. The token math is identical to Collapse,
        // but the summary is attributed to the **requested** action (auto_compact) — NOT silently
        // downgraded to context_collapse by whichever stage did the draining. This is the C fix:
        // op-label == summary/log label (node K04/K09 + the manager-level regression gate).
        let (before, saved, summary, archived, msgs, total) =
            run_baseline(PressureAction::AutoCompact);
        assert_eq!(before, 2000);
        assert_eq!(saved, 1681);
        assert_eq!(archived, 3);
        assert_eq!(msgs, 3);
        assert_eq!(total, 319);
        let summary = summary.expect("auto-compact summarizes the archived messages");
        assert!(
            summary.contains("[Compressed: auto_compact]"),
            "got: {summary}"
        );
    }

    #[test]
    fn baseline_saved_is_monotonic_in_action_level() {
        // The cross-level contract the refactor must preserve: heavier pressure never frees less.
        let snip = run_baseline(PressureAction::SnipCompact).1;
        let micro = run_baseline(PressureAction::MicroCompact).1;
        let collapse = run_baseline(PressureAction::ContextCollapse).1;
        let auto = run_baseline(PressureAction::AutoCompact).1;
        assert!(snip <= micro, "{snip} <= {micro}");
        assert!(micro <= collapse, "{micro} <= {collapse}");
        assert!(collapse <= auto, "{collapse} <= {auto}");
    }

    #[test]
    fn pipeline_stops_cascade_when_target_reached() {
        let cfg = ContextConfig {
            snip_per_msg_ratio: 0.25,
            // preserve_recent_turns=0: no cache-prefix protection, so snip can cap the lone message —
            // this test isolates the cascade early-break (snip reaches target → heavier stages skip).
            preserve_recent_turns: 0,
            ..Default::default()
        };
        let pipeline = CompressionPipeline::new(&cfg);
        let mut ctx = ContextPartitions::new(&cfg);
        ctx.history.push(Message::user("a".repeat(3600)), 900);

        let (saved, summary, archived, _cache_at) =
            pipeline.compress(&mut ctx, PressureAction::AutoCompact, 1_000, 500, &engine());

        assert!(saved > 0);
        assert!(
            summary.is_none(),
            "auto compactor should not run after snip reaches target"
        );
        assert!(
            archived.is_empty(),
            "heavier archival stages should not run"
        );
        assert_eq!(ctx.history.messages.len(), 1);
        assert!(ctx.total_tokens(&engine()) <= 500);
    }
}
