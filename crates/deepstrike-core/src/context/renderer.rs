#[cfg(test)]
use super::fault::stable_hash;
use super::partitions::ContextPartitions;
use super::token_engine::ContextTokenEngine;
use super::units::{strict_tool_pairing_is_valid, unit_boundaries};
use crate::mm::handle::{HandleTable, Residency};
use crate::types::message::{Content, ContentPart, CoreMessage, Role};
use serde::{Deserialize, Serialize};

/// Structured render output aligned with LLM API slots.
///
/// Slot 1 — system_stable:    Identity (system partition). Anthropic system[0] cache_control.
/// Slot 2 — system_knowledge: Knowledge partition. Anthropic system[1] cache_control.
/// Slot 3 — turns[0..N]:      History turns (stable, cacheable prefix).
/// Slot 4 — state_turn:       State (task_state + signals), rebuilt every call.
///
/// The State turn is kept OUT of `turns` so the history prefix stays byte-stable
/// across turns and can be prompt-cached. Providers place `state_turn` themselves:
/// Anthropic appends it AFTER the message-history cache breakpoint (so the volatile
/// state is the cheap uncached tail); OpenAI-family prepend it (preserving today's
/// ordering). When this struct is produced by an older binding that has not been
/// rebuilt, `state_turn` is absent and `turns[0]` still carries the State turn —
/// providers handle both shapes.
///
/// system_text = system_stable + system_knowledge (for OpenAI which has one system slot).
///
/// F3 projection-pair registration (0.2.66): the wire版 `RenderedContext`
/// (runtime/kernel/wire/effect.rs) is the ABI authority; this internal render is the
/// runtime-phase representation. The only legal crossing is the driver's exhaustive
/// projection (`wire/driver.rs::rendered_context`). The `Internal` prefix IS the ruling —
/// host-facing bindings keep the wire name.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InternalRenderedContext {
    /// Identity + Knowledge combined — for providers with a single system slot (OpenAI).
    pub system_text: String,
    /// Identity only (system partition). Anthropic system[0] with cache_control.
    pub system_stable: String,
    /// Knowledge (memory retrievals, skill definitions, artifacts). Anthropic system[1] with cache_control.
    pub system_knowledge: String,
    /// History turns only — the stable, cacheable message prefix.
    pub turns: Vec<CoreMessage>,
    /// Volatile State turn (task_state + signals), rebuilt every call. Rendered
    /// after the cacheable history. `None` when there is no task state or signals.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state_turn: Option<CoreMessage>,
    /// P1-E: number of leading `turns` that form the **frozen prefix** — byte-stable until the
    /// next compaction. Providers that place explicit cache breakpoints (Anthropic) pin one *deep*
    /// breakpoint at this boundary (a long-lived cache that survives many turns and is immune to
    /// the 20-block lookback miss on heavy tool turns) and roll the other at the tail. `None` when
    /// there is no distinct frozen region yet (pre-first-compaction, or the whole render is hot) —
    /// providers then fall back to the rolling-pair placement. Providers clamp out-of-range values.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frozen_prefix_len: Option<usize>,
    /// Explicit evidence that the fixed context or protected tail exceeded the declared input
    /// budget. Hosts must not submit this context unchanged; the state machine uses it to trigger
    /// compaction or terminate with `ContextOverflow`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_overflow: Option<ContextBudgetOverflow>,
    /// Plan task 1.1c · the history item each of `turns` renders, aligned with `turns`; `0` for a
    /// turn that is no item (the synthetic resume anchor, a message without an id).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub turn_items: Vec<crate::context::item::ItemId>,
    /// Plan task 5.1 · the kernel's estimate of the request's input tokens: system, knowledge,
    /// state turn and the selected turns, before tool schemas and calibration are added.
    #[serde(default)]
    pub estimated_tokens: u32,
    /// Plan task 1.1c · the provider replay kept with each turn's item, aligned with `turns`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub turn_replays: Vec<Option<serde_json::Value>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextBudgetOverflowKind {
    FixedContext,
    ProtectedTail,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextBudgetOverflow {
    pub kind: ContextBudgetOverflowKind,
    pub required_tokens: u32,
    pub max_tokens: u32,
}

/// Per-render fingerprint of the **cacheable prefix** — the segments a provider
/// caches as a stable prefix (system blocks + history `turns`). Excludes
/// `state_turn` (the volatile uncached tail) and `token_count` metadata (not on the
/// wire). This is the metrics-first instrument (P0-A) behind the optimization work:
/// two renders share a reusable KV / prompt-cache prefix iff their system hashes
/// match *and* one's `turn_hashes` is a prefix of the other's. Pure and derived —
/// never stored in snapshots, session logs, or event logs.
#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PrefixFingerprint {
    pub system_stable_hash: u64,
    pub system_knowledge_hash: u64,
    /// One stable hash per history turn, in order. The longest common prefix with a
    /// previous render's vector = how many turns stay cache-reusable across the call.
    pub turn_hashes: Vec<u64>,
}

#[cfg(test)]
impl PrefixFingerprint {
    /// True when `self`'s cacheable prefix is a byte-stable *extension* of `prev`:
    /// identical system segments and `prev.turn_hashes` is a prefix of
    /// `self.turn_hashes`. This is exactly the KV / prompt-cache reuse condition —
    /// no drift anywhere in the prefix, only growth at the tail.
    pub(crate) fn extends(&self, prev: &PrefixFingerprint) -> bool {
        self.system_stable_hash == prev.system_stable_hash
            && self.system_knowledge_hash == prev.system_knowledge_hash
            && prev.turn_hashes.len() <= self.turn_hashes.len()
            && self.turn_hashes[..prev.turn_hashes.len()] == prev.turn_hashes[..]
    }

    /// Number of leading turns byte-identical to `prev` — the reusable turn-prefix
    /// length. A drop below `prev.turn_hashes.len()` signals mid-prefix churn (a
    /// turn rewritten in place, e.g. an in-place collapse) that invalidates cache.
    pub(crate) fn common_turn_prefix(&self, prev: &PrefixFingerprint) -> usize {
        self.turn_hashes
            .iter()
            .zip(prev.turn_hashes.iter())
            .take_while(|(a, b)| a == b)
            .count()
    }
}

/// Wire-relevant hash of one turn: role + content + tool_calls, **excluding**
/// `token_count` (kernel-only metadata that never reaches the provider). Serialised
/// through serde so every content variant and tool-call argument is covered with a
/// deterministic field order.
#[cfg(test)]
fn hash_turn(msg: &CoreMessage) -> u64 {
    let material =
        serde_json::to_vec(&(&msg.role, &msg.content, &msg.tool_calls)).unwrap_or_default();
    stable_hash(&material)
}

#[cfg(test)]
impl InternalRenderedContext {
    /// Compute the [`PrefixFingerprint`] for this render. See its docs for the
    /// cache-reuse contract it certifies.
    pub(crate) fn prefix_fingerprint(&self) -> PrefixFingerprint {
        PrefixFingerprint {
            system_stable_hash: stable_hash(self.system_stable.as_bytes()),
            system_knowledge_hash: stable_hash(self.system_knowledge.as_bytes()),
            turn_hashes: self.turns.iter().map(hash_turn).collect(),
        }
    }
}

fn build_system_stable(partitions: &ContextPartitions) -> String {
    partitions
        .system
        .messages()
        .iter()
        .filter_map(|m| m.content.as_text())
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn build_system_knowledge(partitions: &ContextPartitions) -> String {
    partitions
        .knowledge
        .resident_messages()
        .filter_map(|m| m.content.as_text())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Build the State turn (the volatile tail): task_state + signals + the "Proceed." anchor.
fn build_state_turn(partitions: &ContextPartitions) -> Option<CoreMessage> {
    let task = partitions.task_state.format_compact();
    let staged = partitions
        .knowledge
        .staged_messages()
        .filter_map(|m| m.content.as_text())
        .collect::<Vec<_>>()
        .join("\n\n");
    if task.is_empty() && partitions.signals.is_empty() && staged.is_empty() {
        return None;
    }
    let mut parts: Vec<String> = Vec::new();
    if !staged.is_empty() {
        parts.push(staged);
    }
    if !task.is_empty() {
        parts.push(task);
    }
    let signals_text = partitions
        .signals
        .iter()
        .map(|signal| signal.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    if !signals_text.is_empty() {
        parts.push(signals_text);
    }
    let body = parts.join("\n\n");
    Some(CoreMessage::user(format!("{body}\n\nProceed.")))
}

/// Ensure turns start with a user message.
/// After AutoCompact the preserved tail may be all assistant/tool — insert an anchor.
fn normalize_turn_prefix(turns: &mut Vec<CoreMessage>) {
    if !turns.is_empty() && matches!(turns[0].role, Role::Assistant | Role::Tool) {
        turns.insert(0, CoreMessage::user("[context resumed]"));
    }
}

/// Layer-4 read-time projection: replace the body of a `Collapsed` tool result with a short
/// preview, leaving a marker. Non-destructive — the full output stays in `partitions.history`;
/// only the rendered copy shrinks. Un-collapse is boundary-only (P0-C): handles re-evaluate
/// from Resident at the next compaction/renewal, never mid-generation (cache-safe monotonic).
/// Trained-convention truncation marker: says what was cut and how to get it back (the
/// `read_result` tool + this result's `call_id`), instead of kernel-internal vocabulary
/// ("projected out of view") the model has never seen in training.
pub(crate) fn collapse_preview(output: &str, call_id: &str) -> String {
    const PREVIEW_BYTES: usize = 160;
    let retrieval = format!(
        "Call the read_result tool with call_id \"{call_id}\" to re-read the full result.]"
    );
    if output
        .lines()
        .last()
        .is_some_and(|line| line.starts_with("[Output truncated:") && line.ends_with(&retrieval))
    {
        return output.to_string();
    }
    let mut end = PREVIEW_BYTES.min(output.len());
    while end > 0 && !output.is_char_boundary(end) {
        end -= 1;
    }
    let dropped = output.len().saturating_sub(end);
    format!(
        "{}…\n[Output truncated: {dropped} bytes omitted. Call the read_result tool with call_id \"{call_id}\" to re-read the full result.]",
        &output[..end]
    )
}

/// If any tool-result body is not resident in working context (`Collapsed` or `PagedOut`), return
/// a projected copy with those parts previewed; `None` if nothing is projected.
fn project_message(msg: &CoreMessage, handles: &HandleTable) -> Option<CoreMessage> {
    let Content::Parts(parts) = &msg.content else {
        return None;
    };
    let mut changed = false;
    let new_parts: Vec<ContentPart> = parts
        .iter()
        .map(|part| match part {
            ContentPart::ToolResult {
                call_id,
                output,
                is_error,
                durable_content,
            } if matches!(
                handles.residency_for_source(call_id),
                Some(Residency::Collapsed | Residency::PagedOut { .. })
            ) =>
            {
                changed = true;
                ContentPart::ToolResult {
                    call_id: call_id.clone(),
                    output: collapse_preview(output, call_id.as_str()),
                    is_error: *is_error,
                    durable_content: durable_content.clone(),
                }
            }
            other => other.clone(),
        })
        .collect();
    if changed {
        let mut projected = msg.clone();
        projected.content = Content::Parts(new_parts);
        Some(projected)
    } else {
        None
    }
}

/// Render the context into a `InternalRenderedContext` suitable for a provider API call.
///
/// Equivalent to [`render_projected`] with an empty handle table (no Layer-4 projection) and no
/// frozen-prefix boundary (`frozen_history_len = 0` → `frozen_prefix_len` is always `None`).
/// Test convenience — the production path is `ContextManager::render` → [`render_projected`].
#[cfg(test)]
pub(crate) fn render(
    partitions: &ContextPartitions,
    budget: u32,
    engine: &ContextTokenEngine,
    preserve_recent_units: usize,
) -> InternalRenderedContext {
    render_projected(partitions, budget, engine, preserve_recent_units, &HandleTable::new(), 0)
}

/// Render with Layer-4 read-time projection driven by `handles`: tool results whose handle is
/// `Collapsed` or `PagedOut` render as previews (originals untouched), freeing budget for more
/// recent turns.
///
/// Token budget:
///   system_stable + system_knowledge tokens are subtracted first.
///   Remaining budget is allocated to history turns newest-first.
///   The newest protected context units are always included.
///   Every other context unit is included or dropped atomically.
pub fn render_projected(
    partitions: &ContextPartitions,
    budget: u32,
    engine: &ContextTokenEngine,
    preserve_recent_units: usize,
    handles: &HandleTable,
    frozen_history_len: usize,
) -> InternalRenderedContext {
    let system_stable = build_system_stable(partitions);
    let system_knowledge = build_system_knowledge(partitions);
    let system_text = [system_stable.as_str(), system_knowledge.as_str()]
        .iter()
        .filter(|s| !s.is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join("\n\n");

    // Fixed context is accounted before history. Counting the real value (rather than clamping it
    // to the budget) makes an impossible request observable instead of hiding the overage.
    let system_tokens = engine.count(&system_text);
    let state_turn = build_state_turn(partitions);
    let state_tokens = state_turn
        .as_ref()
        .map_or(0, |message| engine.count_message(message));
    let fixed_tokens = system_tokens.saturating_add(state_tokens);
    let mut remaining = budget.saturating_sub(fixed_tokens);
    let mut used_tokens = fixed_tokens;
    let mut budget_overflow = (fixed_tokens > budget).then_some(ContextBudgetOverflow {
        kind: ContextBudgetOverflowKind::FixedContext,
        required_tokens: fixed_tokens,
        max_tokens: budget,
    });

    let units = unit_boundaries(partitions.history.messages());
    let protected_from = units.len().saturating_sub(preserve_recent_units);
    let mut kept_messages_rev = Vec::with_capacity(partitions.history.len());
    let mut kept_unit_ranges = Vec::with_capacity(units.len());
    let mut selected_indices_rev = Vec::with_capacity(partitions.history.len());

    // Plan task 1.1 · an item renders by its fidelity: one whose body a handle has taken out of
    // the view renders as that handle's preview. Items without an id (an older checkpoint) are
    // checked part by part.
    let handle_items = handles.projected_items();
    for (unit_index, unit) in units.iter().enumerate().rev() {
        let is_protected = unit_index >= protected_from;
        let unit_start = kept_messages_rev.len();
        let mut tokens = 0u32;
        for message_index in unit.clone() {
            let msg = &partitions.history.messages()[message_index];
            let item = partitions.history.tag(message_index).id;
            let as_handle = item == 0 || handle_items.contains(&item);
            let projected = as_handle.then(|| project_message(msg, handles)).flatten();
            let effective = projected.clone().unwrap_or_else(|| msg.clone());
            tokens += projected
                .as_ref()
                .map(|_| engine.count_message(&effective))
                .unwrap_or_else(|| partitions.history.measured_tokens(message_index, engine));
            kept_messages_rev.push(effective);
        }
        if tokens == 0 {
            kept_messages_rev.truncate(unit_start);
            continue;
        }

        if is_protected || tokens <= remaining {
            selected_indices_rev.extend(unit.clone());
            kept_unit_ranges.push(unit_start..kept_messages_rev.len());
            remaining = remaining.saturating_sub(tokens);
            used_tokens = used_tokens.saturating_add(tokens);
            if is_protected && used_tokens > budget && budget_overflow.is_none() {
                budget_overflow = Some(ContextBudgetOverflow {
                    kind: ContextBudgetOverflowKind::ProtectedTail,
                    required_tokens: used_tokens,
                    max_tokens: budget,
                });
            }
        } else {
            kept_messages_rev.truncate(unit_start);
            break;
        }
    }

    let mut turns = Vec::with_capacity(kept_messages_rev.len());
    let mut selected_indices = Vec::with_capacity(selected_indices_rev.len());
    for unit in kept_unit_ranges.into_iter().rev() {
        // Units were appended newest-first, so every reverse-ordered range is the current suffix.
        // Draining suffixes restores chronological unit order without cloning messages or allocating
        // one temporary Vec per unit.
        selected_indices.extend(selected_indices_rev.drain(unit.clone()));
        turns.extend(kept_messages_rev.drain(unit));
    }
    let before_anchor = turns.len();
    normalize_turn_prefix(&mut turns);
    let anchor_count = turns.len() - before_anchor;
    let turn_items: Vec<crate::context::item::ItemId> = std::iter::repeat_n(0, anchor_count)
        .chain(selected_indices.iter().map(|index| partitions.history.tag(*index).id))
        .collect();
    debug_assert_eq!(turn_items.len(), turns.len());
    debug_assert!(
        !strict_tool_pairing_is_valid(partitions.history.messages())
            || strict_tool_pairing_is_valid(&turns),
        "renderer split a valid tool transaction"
    );

    // P1-E: locate the frozen-prefix boundary in rendered turns. `frozen_history_len` is the
    // history length as of the last compaction (0 before any) — messages beyond it are the hot
    // tail that grows each turn. Source indices preserve the boundary even when zero-token or
    // budget-omitted turns disappear; a synthetic leading anchor belongs to the frozen region. Emit `Some`
    // only for a distinct, non-empty frozen region; otherwise providers use the rolling-pair
    // fallback (deep == tail would waste a breakpoint).
    let frozen_selected = selected_indices
        .iter()
        .take_while(|index| **index < frozen_history_len)
        .count();
    let frozen_prefix_len = if frozen_selected > 0 && frozen_selected < selected_indices.len() {
        Some(frozen_selected + anchor_count)
    } else {
        None
    };
    // Reuse the fingerprint-validated measurements that admitted the selected units. Recounting
    // every resident body here both discards host measurement provenance and repeats costly BPE
    // over large tool results. Only the synthetic anchor has not already been measured.
    let projected_tokens = turns
        .iter()
        .take(anchor_count)
        .fold(used_tokens, |total, message| {
            total.saturating_add(engine.count_message(message))
        });
    if projected_tokens > budget {
        match &mut budget_overflow {
            None => {
                budget_overflow = Some(ContextBudgetOverflow {
                    kind: ContextBudgetOverflowKind::ProtectedTail,
                    required_tokens: projected_tokens,
                    max_tokens: budget,
                })
            }
            Some(overflow) if overflow.kind == ContextBudgetOverflowKind::ProtectedTail => {
                overflow.required_tokens = projected_tokens;
            }
            Some(_) => {}
        }
    }

    InternalRenderedContext {
        system_text,
        system_stable,
        system_knowledge,
        turns,
        state_turn,
        frozen_prefix_len,
        budget_overflow,
        turn_items,
        estimated_tokens: projected_tokens,
        turn_replays: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::config::ContextConfig;
    use crate::context::partitions::ContextPartitions;
    use crate::context::task_state::TaskState;
    use crate::context::token_engine::ContextTokenEngine;
    use crate::types::message::{CoreMessage, Role};

    fn engine() -> ContextTokenEngine {
        ContextTokenEngine::char_approx()
    }
    fn ctx() -> ContextPartitions {
        ContextPartitions::new(&ContextConfig::default())
    }

    #[test]
    fn frozen_prefix_uses_selected_source_indices_after_zero_token_omission() {
        let mut c = ctx();
        c.history.push(CoreMessage::user("frozen"), 1);
        c.history.push(CoreMessage::user("zero"), 0);
        c.history.push(CoreMessage::user("hot"), 1);
        let rendered = render_projected(&c, 100, &engine(), 1, &HandleTable::new(), 1);
        assert_eq!(rendered.turns.len(), 2);
        assert_eq!(rendered.frozen_prefix_len, Some(1));
    }

    #[test]
    fn system_stable_contains_system_partition() {
        let mut c = ctx();
        c.system.push(CoreMessage::system("You are helpful."), 10);
        let rc = render(&c, 10_000, &engine(), 4);
        assert!(rc.system_stable.contains("You are helpful."));
        assert!(rc.system_text.contains("You are helpful."));
    }

    #[test]
    fn system_knowledge_contains_knowledge_partition() {
        let mut c = ctx();
        c.knowledge.push(CoreMessage::system("skill: debug"), 10);
        let rc = render(&c, 10_000, &engine(), 4);
        assert!(rc.system_knowledge.contains("skill: debug"));
        assert!(rc.system_text.contains("skill: debug"));
    }

    #[test]
    fn task_state_appears_in_state_turn() {
        let mut c = ctx();
        c.task_state = TaskState {
            goal: "find the bug".to_string(),
            ..Default::default()
        };
        let rc = render(&c, 10_000, &engine(), 4);
        assert!(
            !rc.system_text.contains("[TASK STATE]"),
            "task_state must not be in system_text"
        );
        let state = rc.state_turn.as_ref().expect("should have a state turn");
        assert_eq!(state.role, Role::User);
        assert!(
            state
                .content
                .as_text()
                .unwrap()
                .contains("[TASK STATE] goal: find the bug")
        );
        // State is NOT in the cacheable history turns.
        assert!(!rc.turns.iter().any(|m| {
            m.content
                .as_text()
                .map(|t| t.contains("[TASK STATE]"))
                .unwrap_or(false)
        }));
    }

    #[test]
    fn signals_appear_in_state_turn() {
        let mut c = ctx();
        c.task_state = TaskState {
            goal: "g".to_string(),
            ..Default::default()
        };
        c.signals.push(crate::context::partitions::SignalItem::new("[ROLLBACK] tool failed"));
        let rc = render(&c, 10_000, &engine(), 4);
        let state = rc.state_turn.as_ref().unwrap();
        assert!(
            state
                .content
                .as_text()
                .unwrap()
                .contains("[ROLLBACK] tool failed")
        );
    }

    #[test]
    fn empty_task_state_no_state_turn() {
        let c = ctx();
        let rc = render(&c, 10_000, &engine(), 4);
        // No state turn when task_state is empty and no signals
        assert!(rc.state_turn.is_none());
        assert!(rc.turns.is_empty());
    }

    #[test]
    fn history_excludes_state_turn() {
        let mut c = ctx();
        c.task_state = TaskState {
            goal: "g".to_string(),
            ..Default::default()
        };
        c.history.push(CoreMessage::user("step 1"), 5);
        c.history.push(CoreMessage::assistant("done"), 5);
        let rc = render(&c, 10_000, &engine(), 4);
        // turns is history only; state lives in state_turn.
        assert!(
            rc.state_turn
                .as_ref()
                .unwrap()
                .content
                .as_text()
                .unwrap()
                .contains("[TASK STATE]")
        );
        assert_eq!(rc.turns[0].role, Role::User);
        assert_eq!(rc.turns[0].content.as_text(), Some("step 1"));
        assert_eq!(rc.turns[1].role, Role::Assistant);
    }

    #[test]
    fn all_assistant_tool_history_gets_anchor_user_turn() {
        let mut c = ctx();
        c.history.push(CoreMessage::assistant("reply"), 5);
        let rc = render(&c, 10_000, &engine(), 4);
        assert_eq!(rc.turns[0].role, Role::User);
    }

    #[test]
    fn zero_token_messages_skipped() {
        let mut c = ctx();
        c.history.push(CoreMessage::user("zero"), 0);
        c.history.push(CoreMessage::user("real"), 5);
        let rc = render(&c, 10_000, &engine(), 4);
        // Only "real" in history turns (state turn absent — no task_state)
        assert!(rc.turns.iter().any(|m| m.content.as_text() == Some("real")));
        assert!(!rc.turns.iter().any(|m| m.content.as_text() == Some("zero")));
    }

    #[test]
    fn collapsed_tool_result_renders_as_preview_without_mutating_history() {
        use crate::mm::handle::{Handle, HandleKind, HandleTable, Residency};

        let mut c = ctx();
        let long = "DATA ".repeat(200); // 1000 bytes
        c.history.push(
            CoreMessage::tool(vec![ContentPart::ToolResult {
                call_id: "c1".into(),
                output: long.clone(),
                is_error: false,
                durable_content: None,
            }]),
            250,
        );

        let mut handles = HandleTable::new();
        let mut h = Handle::resident_for(1, HandleKind::ToolResult, 250, "c1");
        h.residency = Residency::Collapsed;
        handles.insert(h);

        let rc = render_projected(&c, 10_000, &engine(), 4, &handles, 0);
        let rendered: String = rc
            .turns
            .iter()
            .flat_map(|m| match &m.content {
                Content::Parts(parts) => parts.clone(),
                _ => Vec::new(),
            })
            .find_map(|p| match p {
                ContentPart::ToolResult { output, .. } => Some(output),
                _ => None,
            })
            .expect("tool result rendered");
        // Rendered copy is a preview with the trained truncation phrasing + retrieval instruction.
        assert!(rendered.contains("[Output truncated:"));
        assert!(rendered.contains("read_result"));
        assert!(rendered.contains("\"c1\""));
        assert!(rendered.len() < long.len());
        let stored = match &c.history.messages()[0].content {
            Content::Parts(parts) => match &parts[0] {
                ContentPart::ToolResult { output, .. } => output.clone(),
                _ => unreachable!(),
            },
            _ => unreachable!(),
        };
        assert_eq!(stored, long, "projection must not mutate stored history");
    }

    #[test]
    fn paged_out_tool_result_uses_the_same_projection_as_collapsed() {
        use crate::mm::handle::{Handle, HandleKind, HandleTable, Residency};

        let mut c = ctx();
        let long = "ARCHIVED ".repeat(200);
        c.history.push(
            CoreMessage::tool(vec![ContentPart::ToolResult {
                call_id: "c-paged".into(),
                output: long,
                is_error: false,
                durable_content: None,
            }]),
            300,
        );

        let render_with = |residency| {
            let mut handles = HandleTable::new();
            let mut handle = Handle::resident_for(1, HandleKind::ToolResult, 300, "c-paged");
            handle.residency = residency;
            handles.insert(handle);
            render_projected(&c, 10_000, &engine(), 4, &handles, 0)
        };
        let collapsed = render_with(Residency::Collapsed);
        let paged_out = render_with(Residency::PagedOut {
            payload_ref: "payload:archive-c-paged".to_string(),
            digest: format!("sha256:{}", "1".repeat(64)),
        });

        assert_eq!(
            serde_json::to_value(paged_out.turns).unwrap(),
            serde_json::to_value(collapsed.turns).unwrap(),
            "PagedOut and Collapsed both retain only the read_result preview in working context",
        );
    }

    #[test]
    fn resident_tool_result_renders_in_full() {
        use crate::mm::handle::{Handle, HandleKind, HandleTable};

        let mut c = ctx();
        let body = "RESIDENT BODY ".repeat(20);
        c.history.push(
            CoreMessage::tool(vec![ContentPart::ToolResult {
                call_id: "c2".into(),
                output: body.clone(),
                is_error: false,
                durable_content: None,
            }]),
            60,
        );
        let mut handles = HandleTable::new();
        handles.insert(Handle::resident_for(1, HandleKind::ToolResult, 60, "c2"));

        let rc = render_projected(&c, 10_000, &engine(), 4, &handles, 0);
        let rendered: String = rc
            .turns
            .iter()
            .flat_map(|m| match &m.content {
                Content::Parts(parts) => parts.clone(),
                _ => Vec::new(),
            })
            .find_map(|p| match p {
                ContentPart::ToolResult { output, .. } => Some(output),
                _ => None,
            })
            .expect("tool result rendered");
        assert_eq!(rendered, body);
        assert!(!rendered.contains("[Output truncated:"));
    }

    // ── P1-F: state-turn recency footer ───────────────────────────────────

    // ── P0-A: prefix fingerprint (cache-drift instrument) ──────────────────

    #[test]
    fn prefix_fingerprint_is_stable_when_appending_history() {
        let mut c = ctx();
        c.system.push(CoreMessage::system("rules"), 5);
        c.knowledge.push(CoreMessage::system("skill: debug"), 5);
        c.history.push(CoreMessage::user("turn A"), 5);
        c.history.push(CoreMessage::assistant("turn B"), 5);
        let fp1 = render(&c, 100_000, &engine(), 4).prefix_fingerprint();

        // Append a new turn — the existing prefix must stay byte-identical.
        c.history.push(CoreMessage::user("turn C"), 5);
        let fp2 = render(&c, 100_000, &engine(), 4).prefix_fingerprint();

        assert!(
            fp2.extends(&fp1),
            "appending must only grow the tail, never drift the prefix"
        );
        assert_eq!(
            fp2.common_turn_prefix(&fp1),
            2,
            "both prior turns stay cache-reusable"
        );
        assert_eq!(fp2.turn_hashes.len(), 3);
    }

    #[test]
    fn prefix_fingerprint_ignores_state_turn() {
        // Same history, different task_state/signals → the cacheable prefix is
        // identical (state lives in the uncached tail, out of `turns`).
        let mut c = ctx();
        c.history.push(CoreMessage::user("turn A"), 5);
        c.task_state = TaskState {
            goal: "first goal".to_string(),
            ..Default::default()
        };
        let fp1 = render(&c, 100_000, &engine(), 4).prefix_fingerprint();

        c.task_state = TaskState {
            goal: "totally different goal".to_string(),
            ..Default::default()
        };
        c.signals.push(crate::context::partitions::SignalItem::new("[ROLLBACK] whatever"));
        let fp2 = render(&c, 100_000, &engine(), 4).prefix_fingerprint();

        assert_eq!(
            fp1, fp2,
            "volatile state must not perturb the cacheable prefix"
        );
    }

    #[test]
    fn prefix_fingerprint_detects_system_drift() {
        let mut c = ctx();
        c.system.push(CoreMessage::system("original rules"), 5);
        c.history.push(CoreMessage::user("turn A"), 5);
        let fp1 = render(&c, 100_000, &engine(), 4).prefix_fingerprint();

        c.system.clear();
        c.system.push(CoreMessage::system("updated rules"), 5);
        let fp2 = render(&c, 100_000, &engine(), 4).prefix_fingerprint();

        assert_ne!(fp1.system_stable_hash, fp2.system_stable_hash);
        assert!(
            !fp2.extends(&fp1),
            "a system-block edit invalidates the whole prefix"
        );
    }

    #[test]
    fn prefix_fingerprint_detects_in_place_collapse_churn() {
        use crate::mm::handle::{Handle, HandleKind, HandleTable, Residency};

        let mut c = ctx();
        c.history.push(CoreMessage::user("start"), 5);
        let long = "DATA ".repeat(200);
        c.history.push(
            CoreMessage::tool(vec![ContentPart::ToolResult {
                call_id: "c1".into(),
                output: long,
                is_error: false,
                durable_content: None,
            }]),
            250,
        );
        c.history.push(CoreMessage::user("recent"), 5);

        let resident = render(&c, 100_000, &engine(), 4).prefix_fingerprint();

        // Collapsing the old tool result rewrites that turn in place → the prefix
        // hash at that position changes (the cache-cost of folding, made visible).
        let mut handles = HandleTable::new();
        let mut h = Handle::resident_for(1, HandleKind::ToolResult, 250, "c1");
        h.residency = Residency::Collapsed;
        handles.insert(h);
        let collapsed =
            render_projected(&c, 100_000, &engine(), 4, &handles, 0).prefix_fingerprint();

        // turn 0 ("start") is byte-stable; the collapsed tool result at turn 1 drifts.
        assert_eq!(
            collapsed.common_turn_prefix(&resident),
            1,
            "drift begins at the collapsed turn"
        );
        assert!(!collapsed.extends(&resident));
    }

    #[test]
    fn protected_recent_messages_kept_whole_over_budget() {
        let mut c = ctx();
        c.history.push(CoreMessage::user("first message"), 5);
        c.history.push(CoreMessage::user("a".repeat(1000)), 250);
        // Two protected context units are kept whole regardless of the 10-token budget.
        let rc = render(&c, 10, &engine(), 2);
        assert!(rc.turns.iter().any(|m| {
            m.content
                .as_text()
                .map(|t| t.contains("first message"))
                .unwrap_or(false)
        }));
    }

    #[test]
    fn render_drops_or_keeps_tool_transactions_as_complete_units() {
        let mut c = ctx();
        c.history.push(CoreMessage::user("old"), 10);
        c.history.push(CoreMessage::assistant("old answer"), 10);
        let mut call = CoreMessage::assistant("calling");
        call.tool_calls.push(crate::types::message::ToolCall {
            id: "call-1".into(),
            name: "read".into(),
            arguments: serde_json::json!({}),
        });
        c.history.push(CoreMessage::user("question"), 10);
        c.history.push(call, 10);
        c.history.push(
            CoreMessage::tool(vec![crate::types::message::ContentPart::ToolResult {
                call_id: "call-1".into(),
                output: "ok".into(),
                is_error: false,
                durable_content: None,
            }]),
            10,
        );
        c.history.push(CoreMessage::assistant("answer"), 10);

        let rc = render(&c, 25, &engine(), 1);

        assert_eq!(rc.turns.len(), 4);
        assert_eq!(rc.turns[0].content.as_text(), Some("question"));
        assert_eq!(rc.turns[3].content.as_text(), Some("answer"));
    }

    #[test]
    fn oversized_text_boundary_is_dropped_whole_not_truncated() {
        // P0-B1: an unprotected, over-budget Text boundary message is dropped whole — never
        // mid-truncated — so no budget-dependent fragment lands in the cached prefix.
        let mut c = ctx();
        c.history.push(CoreMessage::user("a".repeat(1000)), 250); // oldest, oversized
        c.history.push(CoreMessage::user("recent"), 2); // newest, fits
        let rc = render(&c, 5, &engine(), 0); // nothing protected
        assert_eq!(rc.turns.len(), 1, "only the fitting newest turn survives");
        assert_eq!(rc.turns[0].content.as_text(), Some("recent"));
        assert!(
            !rc.turns.iter().any(|m| m
                .content
                .as_text()
                .map(|t| t.starts_with("aaaa"))
                .unwrap_or(false)),
            "no truncated body in the prefix"
        );
    }

    #[test]
    fn state_turn_is_budgeted_before_history() {
        let mut c = ctx();
        c.task_state = TaskState {
            goal: "keep the state".to_string(),
            ..Default::default()
        };
        c.history.push(CoreMessage::user("x".repeat(120)), 30);
        let state_tokens = engine().count_message(&build_state_turn(&c).expect("state"));

        let rc = render(&c, state_tokens + 5, &engine(), 0);

        assert!(
            rc.turns.is_empty(),
            "history must not consume the state reservation"
        );
        assert!(rc.budget_overflow.is_none());
    }

    #[test]
    fn synthetic_anchor_overflow_is_reported_at_the_exact_budget_boundary() {
        let mut c = ctx();
        c.history.push(CoreMessage::assistant("reply"), 5);
        let rendered = render_projected(&c, 5, &engine(), 1, &HandleTable::new(), 0);
        assert_eq!(
            rendered.turns[0].content.as_text(),
            Some("[context resumed]")
        );
        let overflow = rendered
            .budget_overflow
            .expect("the required anchor exceeds the budget");
        assert_eq!(overflow.kind, ContextBudgetOverflowKind::ProtectedTail);
        // Fixed context + the synthetic anchor + the accepted host count for the reply.
        let projected_tokens = engine().count(&rendered.system_text)
            + rendered
                .state_turn
                .as_ref()
                .map_or(0, |turn| engine().count_message(turn))
            + engine().count_message(&rendered.turns[0])
            + 5;
        assert_eq!(overflow.required_tokens, projected_tokens);
        assert_eq!(overflow.max_tokens, 5);
        assert!(overflow.required_tokens > 5);

        let within_budget = render(&c, projected_tokens, &engine(), 1);
        assert!(within_budget.budget_overflow.is_none());
        let already_over = render(&c, 1, &engine(), 1).budget_overflow.unwrap();
        assert_eq!(
            already_over.required_tokens, projected_tokens,
            "existing tail-overflow diagnostics must include the anchor too"
        );
    }

    #[test]
    fn protected_tail_overflow_is_reported_instead_of_hidden() {
        let mut c = ctx();
        c.history.push(CoreMessage::user("x".repeat(400)), 100);

        let rc = render(&c, 10, &engine(), 2);

        let overflow = rc
            .budget_overflow
            .expect("protected tail overflow must be explicit");
        assert_eq!(overflow.kind, ContextBudgetOverflowKind::ProtectedTail);
        assert!(overflow.required_tokens > overflow.max_tokens);
    }

    #[test]
    fn fixed_context_overflow_is_reported_with_actual_token_count() {
        let mut c = ctx();
        c.system.push(CoreMessage::system("x".repeat(400)), 100);

        let rc = render(&c, 10, &engine(), 0);

        let overflow = rc
            .budget_overflow
            .expect("fixed context overflow must be explicit");
        assert_eq!(overflow.kind, ContextBudgetOverflowKind::FixedContext);
        assert_eq!(overflow.required_tokens, 100);
        assert_eq!(overflow.max_tokens, 10);
    }
}
