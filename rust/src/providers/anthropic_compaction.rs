//! Server-side compaction for the Anthropic provider, in its two forms (mirrors
//! node/src/providers/anthropic-compaction.ts). Threshold compaction (`compact_20260112`) rides on
//! every request and the API summarizes once input reaches the trigger; with
//! `pause_after_compaction` the API stops after writing the summary so the provider can keep the
//! newest turns verbatim behind it. On-demand compaction (`compact-2026-09-04`) is a separate
//! request the provider sends in the background once a turn's input passes a limit; the signed
//! block it returns replaces the messages it summarized on every later request. Either way the
//! kernel sees an ordinary turn: the provider owns the block, and the kernel's renewal still
//! decides on the full local history.

use futures::future::BoxFuture;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

pub(crate) const THRESHOLD_COMPACTION_BETA: &str = "compact-2026-01-12";
pub(crate) const ON_DEMAND_COMPACTION_BETA: &str = "compact-2026-09-04";
pub(crate) const COMPACT_EDIT: &str = "compact_20260112";

/// Stop a threshold compaction after the summary and keep the newest history messages verbatim
/// behind it. Their thinking blocks are dropped: they were produced with the full history in view
/// and would fail the API's prefix check.
#[derive(Debug, Clone, Copy, Default)]
pub struct AnthropicCompactionPause {
    pub keep_recent_messages: usize,
}

/// On-demand compaction settings (`compact-2026-09-04`).
#[derive(Debug, Clone, Default)]
pub struct AnthropicOnDemandCompaction {
    /// Once a turn's input plus output reaches this many tokens, the provider asks for a summary
    /// of that turn's history in the background and adopts it on the first request after it
    /// arrives. Must be positive.
    pub compact_at_input_tokens: u32,
    /// A summarization prompt that replaces the API's default one (at most 16,384 characters).
    pub instructions: Option<String>,
    /// `max_tokens` for the summarization call; defaults to the turn's own.
    pub max_tokens: Option<u32>,
}

/// The request that continues a paused threshold compaction: the same edits without the pause.
pub(crate) fn without_compaction_pause(body: &Value) -> Value {
    let mut body = body.clone();
    if let Some(edits) = body
        .pointer_mut("/context_management/edits")
        .and_then(Value::as_array_mut)
    {
        for edit in edits {
            if edit.get("type").and_then(Value::as_str) == Some(COMPACT_EDIT)
                && let Some(edit) = edit.as_object_mut()
            {
                edit.remove("pause_after_compaction");
            }
        }
    }
    body
}

pub(crate) fn compaction_block(blocks: &[Value]) -> Option<Value> {
    blocks
        .iter()
        .find(|block| block.get("type").and_then(Value::as_str) == Some("compaction"))
        .cloned()
}

fn content_blocks(message: &Value) -> Vec<Value> {
    match message.get("content") {
        Some(Value::String(text)) if !text.is_empty() => vec![json!({"type": "text", "text": text})],
        Some(Value::Array(blocks)) => blocks.clone(),
        _ => Vec::new(),
    }
}

/// How a paused threshold compaction continues: the continuation's messages and, when recent
/// turns are kept, `(wire index, new content)` of the assistant message the block now heads.
///
/// With `keep_recent_messages` the block heads the nearest assistant message at or before the
/// first kept history message, so the kept turns follow the summary verbatim; without it (or when
/// no assistant message qualifies) the block is appended as an assistant turn of its own, as the
/// API's pause flow does. `history_count` is the number of leading wire messages that are history
/// (the volatile state turn follows them).
pub(crate) fn resume_after_compaction_pause(
    messages: &[Value],
    history_count: usize,
    block: &Value,
    keep_recent_messages: usize,
) -> (Vec<Value>, Option<(usize, Vec<Value>)>) {
    if keep_recent_messages > 0 && history_count > 0 {
        let start = history_count.saturating_sub(keep_recent_messages).min(history_count - 1);
        for index in (1..=start).rev() {
            if messages.get(index).and_then(|m| m.get("role")).and_then(Value::as_str) != Some("assistant") {
                continue;
            }
            let mut content = vec![block.clone()];
            content.extend(
                content_blocks(&messages[index])
                    .into_iter()
                    .filter(|item| {
                        !matches!(
                            item.get("type").and_then(Value::as_str),
                            Some("thinking" | "redacted_thinking")
                        )
                    })
                    .map(|mut item| {
                        if let Some(object) = item.as_object_mut() {
                            object.remove("cache_control");
                        }
                        item
                    }),
            );
            let mut resumed = vec![json!({"role": "assistant", "content": content.clone()})];
            resumed.extend(messages[index + 1..].iter().cloned());
            return (resumed, Some((index, content)));
        }
    }
    let mut resumed = messages.to_vec();
    resumed.push(json!({"role": "assistant", "content": [block.clone()]}));
    (resumed, None)
}

/// Wire values as the API compares them: `cache_control` is a request annotation, not content,
/// and a string message body is the same content as one text block (a cache marker turns the one
/// into the other).
fn stable_json(value: &Value) -> String {
    fn normalize(value: &Value) -> Value {
        match value {
            Value::Object(object) => {
                let string_body = object.get("role").is_some_and(Value::is_string)
                    && object.get("content").is_some_and(Value::is_string);
                let mut out = serde_json::Map::new();
                for (key, item) in object {
                    if key == "cache_control" {
                        continue;
                    }
                    if string_body && key == "content" {
                        out.insert(key.clone(), normalize(&Value::Array(content_blocks(value))));
                    } else {
                        out.insert(key.clone(), normalize(item));
                    }
                }
                Value::Object(out)
            }
            Value::Array(items) => Value::Array(items.iter().map(normalize).collect()),
            other => other.clone(),
        }
    }
    normalize(value).to_string()
}

/// The compaction request a turn's request becomes: the same model, system, tools and thinking,
/// its history messages only, and none of the fields the API rejects on a summarization call.
/// Returns the body and its betas (which travel in the header).
pub(crate) fn on_demand_compaction_body(
    sent: &Value,
    sent_betas: &[String],
    history_count: usize,
    config: &AnthropicOnDemandCompaction,
) -> (Value, Vec<String>) {
    let mut body = sent.clone();
    if let Some(object) = body.as_object_mut() {
        for key in ["stream", "context_management", "stop_sequences"] {
            object.remove(key);
        }
        if matches!(
            object.get("tool_choice").and_then(|c| c.get("type")).and_then(Value::as_str),
            Some("any" | "tool")
        ) {
            object.remove("tool_choice");
        }
        if let Some(Value::Object(output)) = object.get_mut("output_config") {
            output.remove("format");
            if let Some(Value::Object(budget)) = output.get_mut("task_budget") {
                budget.remove("remaining");
            }
            if output.is_empty() {
                object.remove("output_config");
            }
        }
        let messages: Vec<Value> = sent
            .get("messages")
            .and_then(Value::as_array)
            .map(|all| all.iter().take(history_count).cloned().collect())
            .unwrap_or_default();
        object.insert("messages".into(), Value::Array(messages));
        let mut compaction = json!({"type": "summarize"});
        if let Some(instructions) = config.instructions.as_ref().filter(|text| !text.trim().is_empty()) {
            compaction["instructions"] = json!(instructions);
        }
        object.insert("compaction".into(), compaction);
        if let Some(max_tokens) = config.max_tokens {
            object.insert("max_tokens".into(), json!(max_tokens));
        }
    }
    let mut betas: Vec<String> = sent_betas
        .iter()
        .filter(|beta| beta.as_str() != THRESHOLD_COMPACTION_BETA)
        .cloned()
        .collect();
    if !betas.iter().any(|beta| beta == ON_DEMAND_COMPACTION_BETA) {
        betas.push(ON_DEMAND_COMPACTION_BETA.to_owned());
    }
    (body, betas)
}

#[derive(Debug, Clone)]
struct Adoption {
    /// The summarized messages, one digest each, as the compaction request carried them.
    summarized: Vec<String>,
    /// The system prompt and tools the compaction request ran under.
    frame: String,
    /// The assistant message that stands in for the summarized messages.
    message: Value,
}

/// A summary being written: `None` until it settles, then the adoption or `None` for no summary.
type Settled = Arc<Mutex<Option<Option<Adoption>>>>;

#[derive(Default)]
struct CompactorState {
    adoptions: Vec<Adoption>,
    pending: Option<(Settled, Option<tokio::task::JoinHandle<()>>)>,
}

/// Sends one non-streaming compaction request (body, betas) and returns the raw response.
pub(crate) type CompactionSend = Arc<dyn Fn(Value, Vec<String>) -> BoxFuture<'static, crate::Result<Value>> + Send + Sync>;

/// Owns on-demand compaction for one provider: decides when to ask for a summary, runs the
/// request in the background, and swaps the block in for the messages it summarized. Adoptions
/// chain — a later summary covers the earlier block — and every request re-derives the swap from
/// the full history the kernel rendered, so a history the summary no longer matches (a renewal, a
/// changed system prompt or toolset) is sent whole and the stale adoptions are dropped. State is in
/// memory: after a restart the first turn past the limit compacts again.
pub(crate) struct OnDemandCompactor {
    config: AnthropicOnDemandCompaction,
    state: Mutex<CompactorState>,
}

impl OnDemandCompactor {
    pub(crate) fn new(config: AnthropicOnDemandCompaction) -> Self {
        Self { config, state: Mutex::new(CompactorState::default()) }
    }

    /// Swap the adopted summaries into a request body's messages. Returns whether a block was
    /// swapped in; the on-demand beta is then added to `betas`.
    pub(crate) fn apply(&self, body: &mut Value, betas: &mut Vec<String>) -> bool {
        let mut state = self.state.lock().unwrap();
        let settled = state
            .pending
            .as_ref()
            .and_then(|(slot, _)| slot.lock().unwrap().clone());
        if let Some(adoption) = settled {
            state.pending = None;
            if let Some(adoption) = adoption {
                state.adoptions.push(adoption);
            }
        }
        if state.adoptions.is_empty() {
            return false;
        }
        let frame = stable_json(&json!([body.get("system"), body.get("tools")]));
        let mut messages: Vec<Value> = body
            .get("messages")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut applied = 0;
        for adoption in &state.adoptions {
            if adoption.frame != frame || adoption.summarized.len() > messages.len() {
                break;
            }
            if adoption
                .summarized
                .iter()
                .zip(&messages)
                .any(|(digest, message)| &stable_json(message) != digest)
            {
                break;
            }
            let mut swapped = vec![adoption.message.clone()];
            swapped.extend(messages.drain(adoption.summarized.len()..));
            messages = swapped;
            applied += 1;
        }
        state.adoptions.truncate(applied);
        if applied == 0 {
            return false;
        }
        body["messages"] = Value::Array(messages);
        if !betas.iter().any(|beta| beta == ON_DEMAND_COMPACTION_BETA) {
            betas.push(ON_DEMAND_COMPACTION_BETA.to_owned());
        }
        true
    }

    /// After a turn completes: once its input plus output reaches the limit, summarize the history
    /// that turn sent, unless a summary is already being written. Never fails; a summary that does
    /// not come back leaves the history as it is, and the next turn past the limit asks again.
    pub(crate) fn observe(
        &self,
        sent: &Value,
        sent_betas: &[String],
        history_count: usize,
        total_tokens: u32,
        send: CompactionSend,
    ) {
        let mut state = self.state.lock().unwrap();
        if state.pending.is_some()
            || total_tokens < self.config.compact_at_input_tokens
            || history_count == 0
        {
            return;
        }
        let (body, betas) = on_demand_compaction_body(sent, sent_betas, history_count, &self.config);
        let summarized: Vec<String> = body["messages"]
            .as_array()
            .map(|messages| messages.iter().map(stable_json).collect())
            .unwrap_or_default();
        let frame = stable_json(&json!([body.get("system"), body.get("tools")]));
        let slot: Settled = Arc::new(Mutex::new(None));
        let writer = slot.clone();
        let task = tokio::spawn(async move {
            let adoption = match send(body, betas).await {
                Ok(raw) if raw.get("stop_reason").and_then(Value::as_str) == Some("compaction") => raw
                    .get("content")
                    .and_then(Value::as_array)
                    .and_then(|content| compaction_block(content))
                    .map(|block| Adoption {
                        summarized,
                        frame,
                        message: json!({"role": "assistant", "content": [block]}),
                    }),
                _ => None,
            };
            *writer.lock().unwrap() = Some(adoption);
        });
        state.pending = Some((slot, Some(task)));
    }

    /// Returns once any summary in flight has settled. For tests and orderly shutdown.
    pub(crate) async fn idle(&self) {
        let task = self
            .state
            .lock()
            .unwrap()
            .pending
            .as_mut()
            .and_then(|(_, task)| task.take());
        if let Some(task) = task {
            let _ = task.await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_json_ignores_cache_markers_and_string_bodies() {
        let plain = json!({"role": "user", "content": "u2"});
        let marked = json!({"role": "user", "content": [{"type": "text", "text": "u2", "cache_control": {"type": "ephemeral"}}]});
        assert_eq!(stable_json(&plain), stable_json(&marked));
    }

    #[test]
    fn a_paused_compaction_keeps_recent_history_behind_the_block() {
        let block = json!({"type": "compaction", "content": "S."});
        let messages = vec![
            json!({"role": "user", "content": "u1"}),
            json!({"role": "assistant", "content": [
                {"type": "thinking", "thinking": "", "signature": "sig"},
                {"type": "tool_use", "id": "t1", "name": "look", "input": {}},
            ]}),
            json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": "t1", "content": "seen"}]}),
        ];
        let (resumed, anchor) = resume_after_compaction_pause(&messages, 3, &block, 1);
        let expected = vec![block.clone(), json!({"type": "tool_use", "id": "t1", "name": "look", "input": {}})];
        assert_eq!(anchor, Some((1, expected.clone())));
        assert_eq!(resumed[0], json!({"role": "assistant", "content": expected}));
        assert_eq!(resumed.len(), 2);

        let (appended, none) = resume_after_compaction_pause(&messages, 3, &block, 0);
        assert!(none.is_none());
        assert_eq!(appended.last().unwrap(), &json!({"role": "assistant", "content": [block]}));
    }

    #[test]
    fn the_compaction_body_leaves_out_fields_the_api_rejects() {
        let sent = json!({
            "model": "m", "stream": true, "max_tokens": 100,
            "messages": [{"role": "user", "content": "u1"}, {"role": "user", "content": "[TASK STATE]"}],
            "context_management": {"edits": []}, "stop_sequences": ["END"], "tool_choice": {"type": "any"},
            "output_config": {"effort": "high", "format": {}, "task_budget": {"type": "tokens", "total": 30000, "remaining": 1}},
        });
        let config = AnthropicOnDemandCompaction { compact_at_input_tokens: 1, instructions: Some("Keep it.".into()), max_tokens: Some(4096) };
        let (body, betas) = on_demand_compaction_body(&sent, &["compact-2026-01-12".into()], 1, &config);
        assert_eq!(betas, vec![ON_DEMAND_COMPACTION_BETA.to_owned()]);
        assert_eq!(body, json!({
            "model": "m", "max_tokens": 4096,
            "messages": [{"role": "user", "content": "u1"}],
            "output_config": {"effort": "high", "task_budget": {"type": "tokens", "total": 30000}},
            "compaction": {"type": "summarize", "instructions": "Keep it."},
        }));
    }
}
