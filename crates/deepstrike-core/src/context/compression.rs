use super::config::ContextConfig;
use super::partitions::ContextPartitions;
use super::pressure::PressureAction;
use super::summarizer::Summarizer;
use super::token_engine::ContextTokenEngine;
use crate::types::message::{Content, ContentPart, Message};

/// Compression result returned by every compactor.
pub struct CompressResult {
    /// Tokens freed from the partition.
    pub tokens_saved: u32,
    /// Generated summary text if any.
    pub summary: Option<String>,
    /// Messages drained/archived from the context.
    pub archived: Vec<Message>,
}

/// Compression strategy interface.
pub trait Compressor: Send + Sync {
    fn compress(
        &self,
        partitions: &mut ContextPartitions,
        target_tokens: u32,
        max_tokens: u32,
        preserve_k: usize,
        summarizer: &dyn Summarizer,
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
        _preserve_k: usize,
        _summarizer: &dyn Summarizer,
        engine: &ContextTokenEngine,
    ) -> CompressResult {
        let per_msg_limit = ((max_tokens as f64 * self.per_msg_ratio) as u32).max(50);
        let mut saved = 0u32;
        let partition = &mut partitions.history;
        let indices = oversized_text_message_indices(&partition.messages, per_msg_limit, engine);

        for &i in &indices {
            let msg = &mut partition.messages[i];
            let original_tokens = msg.token_count.unwrap_or_else(|| engine.count_message(msg));
            if let Content::Text(ref t) = msg.content {
                let head_limit = per_msg_limit / 2;
                let tail_limit = per_msg_limit.saturating_sub(head_limit);
                let head_text = engine.truncate(t, head_limit);

                let chars: Vec<char> = t.chars().collect();
                let mut low = head_text.chars().count();
                let mut high = chars.len();
                let mut suffix_start = chars.len();
                while low <= high {
                    let mid = (low + high) / 2;
                    if mid >= chars.len() {
                        break;
                    }
                    let candidate: String = chars[mid..].iter().collect();
                    let tokens = engine.count(&candidate);
                    if tokens <= tail_limit {
                        suffix_start = mid;
                        if mid == 0 {
                            break;
                        }
                        high = mid - 1;
                    } else {
                        low = mid + 1;
                    }
                }
                let tail_text: String = chars[suffix_start..].iter().collect();
                let omitted = original_tokens
                    .saturating_sub(head_limit)
                    .saturating_sub(tail_limit);
                msg.content = Content::Text(format!(
                    "{}… [… {} tokens omitted …] …{}",
                    head_text, omitted, tail_text
                ));
                msg.token_count = Some(per_msg_limit);
                saved += original_tokens.saturating_sub(per_msg_limit);
            }
        }

        partition.token_count = partition.token_count.saturating_sub(saved);

        // Pure executor: snip caps oversized messages in place; it never archives or summarizes.
        // Summary + compression-log attribution is the pipeline's job (under the *requested* action).
        CompressResult {
            tokens_saved: saved,
            summary: None,
            archived: vec![],
        }
    }
}

/// Pure selection (W1-1 collapse): indices of oversized **text** history messages a snip caps
/// (tokens > `per_msg_limit`; non-text and tiny ≤10-token messages skipped). The cache-aware planner
/// reuses this to choose which — and how far back — to snip; the executor only applies the head/tail
/// truncation to the chosen indices.
fn oversized_text_message_indices(
    messages: &[Message],
    per_msg_limit: u32,
    engine: &ContextTokenEngine,
) -> Vec<usize> {
    messages
        .iter()
        .enumerate()
        .filter(|(_, msg)| {
            if !matches!(msg.content, Content::Text(_)) {
                return false;
            }
            let toks = msg.token_count.unwrap_or_else(|| engine.count_message(msg));
            toks > per_msg_limit && toks > 10
        })
        .map(|(i, _)| i)
        .collect()
}

/// 获取当前UTC时间戳
fn utc_now() -> String {
    // 在实际使用中，这应该从ProviderResult.now_ms获取
    // 这里简化为占位符
    format!("{:?}", std::time::SystemTime::now())
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
    let total_tokens = engine.count(text);
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

/// Pure selection (W1-1 collapse): indices of history messages whose large (≥200-token) tool result
/// a micro-compact would excerpt — the first tool-result part whose `call_id` is not in
/// `preserved_refs`. The executor applies the excerpt. The cache-aware planner reuses this: tool
/// results are interleaved mid/late history, so excerpting them is prefix-safe.
fn excerptable_tool_result_indices(
    messages: &[Message],
    preserved_refs: &[String],
    engine: &ContextTokenEngine,
) -> Vec<usize> {
    messages
        .iter()
        .enumerate()
        .filter_map(|(i, msg)| {
            let toks = msg.token_count.unwrap_or_else(|| engine.count_message(msg));
            if toks < 200 {
                return None;
            }
            let Content::Parts(parts) = &msg.content else {
                return None;
            };
            let call_id = parts.iter().find_map(|p| match p {
                ContentPart::ToolResult { call_id, .. } => Some(call_id.to_string()),
                _ => None,
            })?;
            (!preserved_refs.contains(&call_id)).then_some(i)
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
        _preserve_k: usize,
        _summarizer: &dyn Summarizer,
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

        // Selection lifted to a pure helper (already excludes `preserved_refs`); the executor only
        // applies the excerpt to the chosen tool-result messages.
        let indices = excerptable_tool_result_indices(
            &partitions.history.messages,
            &partitions.task_state.preserved_refs,
            engine,
        );
        let messages_clone = partitions.history.messages.clone();
        let partition = &mut partitions.history;
        let mut saved = 0u32;

        for &i in &indices {
            let msg = &mut partition.messages[i];
            let original_tokens = msg.token_count.unwrap_or_else(|| engine.count_message(msg));
            if let Content::Parts(ref mut parts) = msg.content {
                let tool_result_index = parts
                    .iter()
                    .position(|p| matches!(p, ContentPart::ToolResult { .. }));
                if let Some(idx) = tool_result_index {
                    if let ContentPart::ToolResult {
                        call_id,
                        output,
                        is_error: _,
                    } = &mut parts[idx]
                    {
                        let tool_name = find_tool_name(call_id, &messages_clone)
                            .unwrap_or_else(|| "unknown".to_string());

                        let new_output = if original_tokens > 2000 {
                            if let Some(json_excerpt) = extract_json_excerpt(output) {
                                format!(
                                    "[tool result: {} | {} | {} tokens]\n{}",
                                    call_id, tool_name, original_tokens, json_excerpt
                                )
                            } else {
                                let excerpt = excerpt_text(output, 30, 10, engine);
                                format!(
                                    "[tool result: {} | {} | {} tokens]\n{}",
                                    call_id, tool_name, original_tokens, excerpt
                                )
                            }
                        } else {
                            let excerpt = excerpt_text(output, 150, 50, engine);
                            format!(
                                "[tool result: {} | {} | {} tokens]\n{}",
                                call_id, tool_name, original_tokens, excerpt
                            )
                        };

                        let new_tokens = engine.count(&new_output);
                        msg.content = Content::Text(new_output);
                        msg.token_count = Some(new_tokens);
                        saved += original_tokens.saturating_sub(new_tokens);
                    }
                }
            }
        }

        partition.token_count = partition.token_count.saturating_sub(saved);

        // Pure executor: excerpts tool results in place; no archive, summary, or self-log.
        CompressResult {
            tokens_saved: saved,
            summary: None,
            archived: vec![],
        }
    }
}

/// Pure **selection** (W1-1 collapse): how many of the oldest history messages to drop to bring the
/// partition under `target_tokens`, never crossing the preserve-recent floor (`keep` messages).
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
    let limit = messages.len().saturating_sub(keep);
    let mut saved = 0u32;
    let mut n = 0usize;
    for (i, msg) in messages.iter().take(limit).enumerate() {
        if total_tokens.saturating_sub(saved) <= target_tokens {
            break;
        }
        saved += msg.token_count.unwrap_or_else(|| engine.count_message(msg));
        n = i + 1;
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
        _summarizer: &dyn Summarizer,
        engine: &ContextTokenEngine,
    ) -> CompressResult {
        let partition = &mut partitions.history;
        let keep = preserve_k * 2; // turns → messages (user + assistant per turn)
        let (n, saved) =
            plan_drop_oldest(&partition.messages, partition.token_count, target_tokens, keep, engine);

        if n == 0 {
            return CompressResult {
                tokens_saved: 0,
                summary: None,
                archived: vec![],
            };
        }

        let archived: Vec<Message> = partition.messages.drain(..n).collect();
        partition.token_count = partition.token_count.saturating_sub(saved);

        // Pure executor: return the drained messages; the pipeline summarizes + logs once under the
        // requested action.
        CompressResult {
            tokens_saved: saved,
            summary: None,
            archived,
        }
    }
}

/// rho > auto_threshold: collapse history entirely except last K turns, updating compression log.
pub struct AutoCompactor;

impl Compressor for AutoCompactor {
    fn compress(
        &self,
        partitions: &mut ContextPartitions,
        _target_tokens: u32,
        _max_tokens: u32,
        preserve_k: usize,
        _summarizer: &dyn Summarizer,
        engine: &ContextTokenEngine,
    ) -> CompressResult {
        let partition = &mut partitions.history;
        if partition.messages.is_empty() {
            return CompressResult {
                tokens_saved: 0,
                summary: None,
                archived: vec![],
            };
        }

        let original_tokens = partition.token_count;
        let keep = preserve_k * 2;
        let limit = partition.messages.len().saturating_sub(keep);
        let (archived, kept): (Vec<Message>, Vec<Message>) = if limit > 0 {
            let archived_msgs = partition.messages.drain(..limit).collect();
            let kept_msgs = partition.messages.drain(..).collect();
            (archived_msgs, kept_msgs)
        } else {
            (vec![], partition.messages.drain(..).collect())
        };

        if archived.is_empty() {
            partition.messages = kept;
            return CompressResult {
                tokens_saved: 0,
                summary: None,
                archived: vec![],
            };
        }

        partition.messages = kept;

        let kept_tokens: u32 = partition
            .messages
            .iter()
            .map(|m| m.token_count.unwrap_or_else(|| engine.count_message(m)))
            .sum();
        partition.token_count = kept_tokens;

        // Pure executor: return the drained messages; the pipeline summarizes + logs once under the
        // requested action.
        CompressResult {
            tokens_saved: original_tokens.saturating_sub(kept_tokens),
            summary: None,
            archived,
        }
    }
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
    ) -> (u32, Option<String>, Vec<Message>) {
        if action == PressureAction::None {
            return (0, None, vec![]);
        }

        let mut total_saved = 0;
        let mut all_archived = vec![];
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
                    &summarizer,
                    engine,
                );
                total_saved += res.tokens_saved;
                all_archived.extend(res.archived);
            }
        }

        // Single decision point for summary + log attribution: whatever the cascade drained is
        // summarized ONCE under the **requested** action and logged once. The compactors are pure
        // executors that no longer self-attribute — so a `compress(AutoCompact)` whose draining
        // happened in the Collapse stage is still labeled `auto_compact` (the C fix), and a
        // `compress(ContextCollapse)` stays `context_collapse` (unchanged).
        let summary = if all_archived.is_empty() {
            None
        } else {
            let s = summarizer.summarize(&all_archived, action, target_tokens);
            partitions.task_state.log_compression(action.label(), s.clone());
            Some(s)
        };

        (total_saved, summary, all_archived)
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
    fn summarizer() -> super::super::summarizer::RuleSummarizer {
        super::super::summarizer::RuleSummarizer
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
        let result = compactor.compress(&mut ctx, 0, MAX, 2, &summarizer(), &engine());
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
        let result = compactor.compress(&mut ctx, 0, MAX, 2, &summarizer(), &engine());
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

        let result = compactor.compress(&mut ctx, 0, MAX, 2, &summarizer(), &engine());
        assert!(result.tokens_saved > 0);
        let text = ctx.history.messages[0].content.as_text().unwrap();
        assert!(
            text.contains("[tool result: c1 | unknown | 300 tokens]"),
            "got: {text}"
        );
    }

    #[test]
    fn collapse_compactor_drops_oldest_to_reach_target() {
        let compactor = CollapseCompactor;
        let mut ctx = ContextPartitions::new(&config());
        for _ in 0..8 {
            ctx.history.push(Message::user("msg"), 50);
        }
        let result = compactor.compress(&mut ctx, 250, MAX, 2, &summarizer(), &engine());
        assert!(result.tokens_saved > 0);
        assert!(ctx.history.messages.len() < 8);
        // Pure executor: returns the drained messages; summary + log attribution is the pipeline's
        // job (under the requested action), so the compactor itself no longer summarizes or logs.
        assert!(!result.archived.is_empty(), "drained messages are returned to the pipeline");
        assert!(result.summary.is_none(), "compactor no longer self-summarizes");
        assert!(ctx.task_state.compression_log.is_empty(), "compactor no longer logs");
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
        assert!(summary.contains("2 messages / 11 tokens archived"));
        assert!(summary.contains("last assistant output: world"));
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

        let result = compactor.compress(&mut ctx, 0, MAX, 2, &summarizer(), &engine());
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
        let result = compactor.compress(&mut ctx, 0, MAX, 2, &summarizer(), &engine());
        assert!(result.tokens_saved > 0);
        assert_eq!(ctx.history.messages.len(), 4); // kept last 2 turns = 4 messages
        // Pure executor: returns the drained messages; the pipeline summarizes + logs under the
        // requested action (see `baseline_auto_*` / `pipeline_attributes_summary_to_requested_action`).
        assert!(!result.archived.is_empty(), "drained messages returned to the pipeline");
        assert!(result.summary.is_none(), "compactor no longer self-summarizes");
        assert!(ctx.task_state.compression_log.is_empty(), "compactor no longer logs");
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
        let (saved, summary, archived) =
            CompressionPipeline::new(&config()).compress(&mut ctx, action, MAX, 500, &engine());
        let archived_len = archived.len();
        let msgs_after = ctx.history.messages.len();
        let total_after = ctx.total_tokens(&engine());
        (before, saved, summary, archived_len, msgs_after, total_after)
    }

    #[test]
    fn baseline_snip_only_caps_text_no_archival() {
        // SnipCompact runs only the Snip stage: caps oversized text messages in place; never
        // archives or summarizes. Cascade stops above target (snip alone can't reach 500).
        let (before, saved, summary, archived, msgs, total) = run_baseline(PressureAction::SnipCompact);
        assert_eq!(before, 2001);
        assert_eq!(saved, 1000);
        assert_eq!(archived, 0);
        assert!(summary.is_none());
        assert_eq!(msgs, 6, "snip mutates in place, drops no messages");
        assert_eq!(total, 1001);
    }

    #[test]
    fn baseline_micro_excerpts_tool_results() {
        // MicroCompact runs Snip then Micro: tool results excerpted to placeholders. Still no
        // archival/summary; messages stay in place.
        let (before, saved, summary, archived, msgs, total) = run_baseline(PressureAction::MicroCompact);
        assert_eq!(before, 2001);
        assert_eq!(saved, 1362);
        assert_eq!(archived, 0);
        assert!(summary.is_none());
        assert_eq!(msgs, 6);
        assert_eq!(total, 639);
    }

    #[test]
    fn baseline_collapse_drops_oldest_and_summarizes() {
        // ContextCollapse runs Snip→Micro→Collapse: oldest messages drained to `archived` with a
        // summary, down to the preserve-recent floor (4 msgs kept).
        let (before, saved, summary, archived, msgs, total) =
            run_baseline(PressureAction::ContextCollapse);
        assert_eq!(before, 2001);
        assert_eq!(saved, 1462);
        assert_eq!(archived, 2, "drops the 2 oldest messages above the preserve floor");
        assert_eq!(msgs, 4, "preserve_recent_turns=2 → 4 messages kept");
        assert_eq!(total, 539);
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
        let (before, saved, summary, archived, msgs, total) = run_baseline(PressureAction::AutoCompact);
        assert_eq!(before, 2001);
        assert_eq!(saved, 1462);
        assert_eq!(archived, 2);
        assert_eq!(msgs, 4);
        assert_eq!(total, 539);
        let summary = summary.expect("auto-compact summarizes the archived messages");
        assert!(summary.contains("[Compressed: auto_compact]"), "got: {summary}");
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
            ..Default::default()
        };
        let pipeline = CompressionPipeline::new(&cfg);
        let mut ctx = ContextPartitions::new(&cfg);
        ctx.history.push(Message::user("a".repeat(3600)), 900);

        let (saved, summary, archived) = pipeline.compress(
            &mut ctx,
            PressureAction::AutoCompact,
            1_000,
            500,
            &engine(),
        );

        assert!(saved > 0);
        assert!(summary.is_none(), "auto compactor should not run after snip reaches target");
        assert!(archived.is_empty(), "heavier archival stages should not run");
        assert_eq!(ctx.history.messages.len(), 1);
        assert!(ctx.total_tokens(&engine()) <= 500);
    }
}
