#[cfg(test)]
use super::fault::stable_hash;
use super::partitions::ContextPartitions;
use super::task_state::TaskState;
use super::token_engine::ContextTokenEngine;
use super::units::{strict_tool_pairing_is_valid, unit_boundaries};
use crate::mm::handle::{HandleTable, Residency};
use crate::types::message::{Content, ContentPart, Message, Role};
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenderedContext {
    /// Identity + Knowledge combined — for providers with a single system slot (OpenAI).
    pub system_text: String,
    /// Identity only (system partition). Anthropic system[0] with cache_control.
    pub system_stable: String,
    /// Knowledge (memory retrievals, skill definitions, artifacts). Anthropic system[1] with cache_control.
    pub system_knowledge: String,
    /// History turns only — the stable, cacheable message prefix.
    pub turns: Vec<Message>,
    /// Volatile State turn (task_state + signals), rebuilt every call. Rendered
    /// after the cacheable history. `None` when there is no task state or signals.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state_turn: Option<Message>,
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
fn hash_turn(msg: &Message) -> u64 {
    let material =
        serde_json::to_vec(&(&msg.role, &msg.content, &msg.tool_calls)).unwrap_or_default();
    stable_hash(&material)
}

#[cfg(test)]
impl RenderedContext {
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
        .messages
        .iter()
        .filter_map(|m| m.content.as_text())
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn build_system_knowledge(partitions: &ContextPartitions) -> String {
    partitions
        .knowledge
        .messages()
        .filter_map(|m| m.content.as_text())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// P1-F (+ 2b/2c): a one-line recency footer at the *last* content before the "Proceed." anchor —
/// the highest-attention position in the prompt (the model attends most to the final tokens).
///
/// It LEADS WITH FORWARD MOTION (what just happened · what to do next · the standing directive), not
/// a verbatim restatement of the goal. Re-injecting the bare goal at this peak-attention slot every
/// turn primes the model to *re-narrate intent* ("好的，我来将<goal>…") instead of acting — an
/// undamped repetition trap when there is no plan/progress to advance. The full goal still LEADS the
/// TASK STATE block above (primacy + reference), so goal-adherence is preserved; the footer restates
/// the goal only when nothing has happened yet (e.g. turn 1, no actions). `None` when there is no goal.
///
/// The "just did" clause is kernel-derived from `recent_actions` (real tool activity), and a trailing
/// run of an identical action raises an explicit STOP — a cheap no-progress backstop that breaks the
/// read→re-read→re-narrate loop in-band, at the position the model weights most.
fn salience_footer(ts: &TaskState) -> Option<String> {
    if ts.goal.is_empty() {
        return None;
    }
    let mut clauses: Vec<String> = Vec::new();

    // What just happened — display tool NAMES only. The full `name(args)` signatures are kept in
    // `recent_actions` for the repeat check below, but rendering them every turn bloats the volatile
    // footer; the names alone show motion at the peak-attention slot.
    let recent = ts.recent_actions.as_slice();
    let action_name = |entry: &str| entry.split('(').next().unwrap_or(entry).to_string();
    if let Some(last) = recent.last() {
        let start = recent.len().saturating_sub(3);
        let names = recent[start..]
            .iter()
            .map(|e| action_name(e))
            .collect::<Vec<_>>()
            .join(" → ");
        clauses.push(format!("did: {names}"));

        // No-progress backstop: the SAME call — name AND args — repeated on the last ≥2 turns is a
        // stall (a legit loop varies its args, so it reads as distinct progress, not a repeat).
        let trailing_repeat = recent.iter().rev().take_while(|a| *a == last).count();
        if trailing_repeat >= 2 {
            clauses.push(format!(
                "STOP: `{}` repeated {trailing_repeat}× unchanged — do something different or report",
                action_name(last)
            ));
        }
    }

    // What to do next — the active plan step if the model maintains one, else a short forward nudge.
    let active_step = ts
        .current_step
        .and_then(|i| ts.plan.get(i).map(|s| (i, s)))
        .filter(|(_, s)| !s.done);
    if let Some((i, step)) = active_step {
        clauses.push(format!("next: step {} — {}", i + 1, step.label));
    } else if !recent.is_empty() {
        clauses.push("next: advance the goal".to_string());
    }

    if let Some(d) = ts.directives.last() {
        clauses.push(format!("must: {d}"));
    }

    // Lead with the goal only when no forward clause fills the footer (turn 1, nothing done yet);
    // otherwise the forward clauses carry the salience and the goal stays in the block above.
    let body = if clauses.is_empty() {
        format!("→ focus: {}", ts.goal)
    } else {
        format!("→ {}", clauses.join(" · "))
    };
    Some(body)
}

/// Build the State turn (the volatile tail): task_state + signals + a recency focus footer +
/// "Proceed." anchor. The footer sits last (just before "Proceed.") so the current goal/step/
/// directive land in the prompt's highest-attention position (P1-F).
fn build_state_turn(partitions: &ContextPartitions) -> Option<Message> {
    let task = partitions.task_state.format_compact();
    if task.is_empty() && partitions.signals.is_empty() {
        return None;
    }
    let mut parts: Vec<String> = Vec::new();
    if !task.is_empty() {
        parts.push(task);
    }
    let signals_text = partitions.signals.join("\n");
    if !signals_text.is_empty() {
        parts.push(signals_text);
    }
    if let Some(footer) = salience_footer(&partitions.task_state) {
        parts.push(footer);
    }
    let body = parts.join("\n\n");
    Some(Message::user(format!("{body}\n\nProceed.")))
}

/// Ensure turns start with a user message.
/// After AutoCompact the preserved tail may be all assistant/tool — insert an anchor.
fn normalize_turn_prefix(turns: &mut Vec<Message>) {
    if !turns.is_empty() && matches!(turns[0].role, Role::Assistant | Role::Tool) {
        turns.insert(0, Message::user("[context resumed]"));
    }
}

/// Layer-4 read-time projection: replace the body of a `Collapsed` tool result with a short
/// preview, leaving a marker. Non-destructive — the full output stays in `partitions.history`;
/// only the rendered copy shrinks. Un-collapse is boundary-only (P0-C): handles re-evaluate
/// from Resident at the next compaction/renewal, never mid-generation (cache-safe monotonic).
fn collapse_preview(output: &str) -> String {
    const PREVIEW_BYTES: usize = 160;
    let mut end = PREVIEW_BYTES.min(output.len());
    while end > 0 && !output.is_char_boundary(end) {
        end -= 1;
    }
    let dropped = output.len().saturating_sub(end);
    format!(
        "{}…\n[collapsed: {dropped} chars projected out of view; full result retained in history]",
        &output[..end]
    )
}

/// Stub substituted for a collapsed assistant preamble. Carries no goal text (that would re-seed the
/// very repetition this removes) and points the model at the authoritative State turn instead.
const NARRATION_STUB: &str = "[earlier narration collapsed; tool call(s) preserved below — current progress is in the TASK STATE block]";

/// Minimum narration length (chars, CJK-aware) worth collapsing. Short preambles aren't worth a
/// stub substitution (and the one-time cache churn it costs as the turn ages out of the window).
const NARRATION_COLLAPSE_MIN_CHARS: usize = 40;

/// Method 1: read-time collapse of an OLD assistant turn's narration. Targets exactly the
/// "preamble before action" turns — `Role::Assistant`, a `Content::Text` body, AND a non-empty
/// `tool_calls` (the model narrated intent, then acted). Returns a projected copy whose text is
/// replaced by [`NARRATION_STUB`] while `tool_calls` (and thus tool_use/tool_result pairing) are
/// left intact; the original full text stays in `partitions.history`, so the projection reverses if
/// the flag is turned off. `None` when the message isn't a collapsible narration turn or the flag is
/// off. Caller restricts this to messages already past the protected recent window.
fn project_assistant_narration(msg: &Message, enabled: bool) -> Option<Message> {
    if !enabled || msg.role != Role::Assistant || msg.tool_calls.is_empty() {
        return None;
    }
    let Content::Text(text) = &msg.content else {
        return None;
    };
    if text == NARRATION_STUB || text.chars().count() < NARRATION_COLLAPSE_MIN_CHARS {
        return None;
    }
    let mut projected = msg.clone();
    projected.content = Content::Text(NARRATION_STUB.to_string());
    projected.token_count = None; // recomputed against the smaller stub
    Some(projected)
}

/// If any of `msg`'s tool-result parts is `Collapsed` per the handle table, return a projected
/// copy with those parts previewed; `None` if nothing is collapsed (render the message as-is).
fn project_message(msg: &Message, handles: &HandleTable) -> Option<Message> {
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
            } if matches!(
                handles.residency_for_source(call_id),
                Some(Residency::Collapsed)
            ) =>
            {
                changed = true;
                ContentPart::ToolResult {
                    call_id: call_id.clone(),
                    output: collapse_preview(output),
                    is_error: *is_error,
                }
            }
            other => other.clone(),
        })
        .collect();
    if changed {
        let mut projected = msg.clone();
        projected.content = Content::Parts(new_parts);
        projected.token_count = None; // recomputed against the smaller projected body
        Some(projected)
    } else {
        None
    }
}

/// Render the context into a `RenderedContext` suitable for a provider API call.
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
) -> RenderedContext {
    // The convenience wrapper renders history verbatim (no narration collapse) — callers that want
    // Method-1 collapse drive `render_projected` with the flag (the kernel passes it from config).
    render_projected(
        partitions,
        budget,
        engine,
        preserve_recent_units,
        &HandleTable::new(),
        0,
        false,
    )
}

/// Render with Layer-4 read-time projection driven by `handles`: tool results whose handle is
/// `Collapsed` render as previews (originals untouched), freeing budget for more recent turns.
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
    collapse_narration: bool,
) -> RenderedContext {
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

    let units = unit_boundaries(&partitions.history.messages);
    let protected_from = units.len().saturating_sub(preserve_recent_units);
    let mut kept_units_rev: Vec<Vec<Message>> = Vec::new();

    for (unit_index, unit) in units.iter().enumerate().rev() {
        let is_protected = unit_index >= protected_from;
        let effective = partitions.history.messages[unit.clone()]
            .iter()
            .map(|msg| {
                project_message(msg, handles)
                    .or_else(|| {
                        if is_protected {
                            None
                        } else {
                            project_assistant_narration(msg, collapse_narration)
                        }
                    })
                    .unwrap_or_else(|| msg.clone())
            })
            .collect::<Vec<_>>();
        let tokens = effective
            .iter()
            .map(|msg| msg.token_count.unwrap_or_else(|| engine.count_message(msg)))
            .sum::<u32>();
        if tokens == 0 {
            continue;
        }

        if is_protected || tokens <= remaining {
            kept_units_rev.push(effective);
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
            break;
        }
    }

    kept_units_rev.reverse();
    let mut turns = kept_units_rev.into_iter().flatten().collect::<Vec<_>>();
    normalize_turn_prefix(&mut turns);
    debug_assert!(
        !strict_tool_pairing_is_valid(&partitions.history.messages)
            || strict_tool_pairing_is_valid(&turns),
        "renderer split a valid tool transaction"
    );

    // P1-E: locate the frozen-prefix boundary in rendered turns. `frozen_history_len` is the
    // history length as of the last compaction (0 before any) — messages beyond it are the hot
    // tail that grows each turn. We count the hot tail from the END, which is robust to the leading
    // anchor and to budget-dropping of OLD turns (the recent tail is never dropped). Emit `Some`
    // only for a distinct, non-empty frozen region; otherwise providers use the rolling-pair
    // fallback (deep == tail would waste a breakpoint).
    let hot = partitions
        .history
        .messages
        .len()
        .saturating_sub(frozen_history_len);
    let frozen_prefix_len = if frozen_history_len > 0 && hot > 0 && hot < turns.len() {
        Some(turns.len() - hot)
    } else {
        None
    };

    RenderedContext {
        system_text,
        system_stable,
        system_knowledge,
        turns,
        state_turn,
        frozen_prefix_len,
        budget_overflow,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::config::ContextConfig;
    use crate::context::partitions::ContextPartitions;
    use crate::context::task_state::{PlanStep, TaskState};
    use crate::context::token_engine::ContextTokenEngine;
    use crate::types::message::{Message, Role};

    fn engine() -> ContextTokenEngine {
        ContextTokenEngine::char_approx()
    }
    fn ctx() -> ContextPartitions {
        ContextPartitions::new(&ContextConfig::default())
    }

    #[test]
    fn system_stable_contains_system_partition() {
        let mut c = ctx();
        c.system.push(Message::system("You are helpful."), 10);
        let rc = render(&c, 10_000, &engine(), 4);
        assert!(rc.system_stable.contains("You are helpful."));
        assert!(rc.system_text.contains("You are helpful."));
    }

    #[test]
    fn system_knowledge_contains_knowledge_partition() {
        let mut c = ctx();
        c.knowledge.push(Message::system("skill: debug"), 10);
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
        c.signals.push("[ROLLBACK] tool failed".to_string());
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
        c.history.push(Message::user("step 1"), 5);
        c.history.push(Message::assistant("done"), 5);
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
        c.history.push(Message::assistant("reply"), 5);
        let rc = render(&c, 10_000, &engine(), 4);
        assert_eq!(rc.turns[0].role, Role::User);
    }

    #[test]
    fn zero_token_messages_skipped() {
        let mut c = ctx();
        c.history.push(Message::user("zero"), 0);
        c.history.push(Message::user("real"), 5);
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
            Message::tool(vec![ContentPart::ToolResult {
                call_id: "c1".into(),
                output: long.clone(),
                is_error: false,
            }]),
            250,
        );

        let mut handles = HandleTable::new();
        let mut h = Handle::resident_for(1, HandleKind::ToolResult, 250, "c1");
        h.residency = Residency::Collapsed;
        handles.insert(h);

        let rc = render_projected(&c, 10_000, &engine(), 4, &handles, 0, false);
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
        // Rendered copy is a preview; original full output is retained in history.
        assert!(rendered.contains("[collapsed:"));
        assert!(rendered.len() < long.len());
        let stored = match &c.history.messages[0].content {
            Content::Parts(parts) => match &parts[0] {
                ContentPart::ToolResult { output, .. } => output.clone(),
                _ => unreachable!(),
            },
            _ => unreachable!(),
        };
        assert_eq!(stored, long, "projection must not mutate stored history");
    }

    #[test]
    fn resident_tool_result_renders_in_full() {
        use crate::mm::handle::{Handle, HandleKind, HandleTable};

        let mut c = ctx();
        let body = "RESIDENT BODY ".repeat(20);
        c.history.push(
            Message::tool(vec![ContentPart::ToolResult {
                call_id: "c2".into(),
                output: body.clone(),
                is_error: false,
            }]),
            60,
        );
        let mut handles = HandleTable::new();
        handles.insert(Handle::resident_for(1, HandleKind::ToolResult, 60, "c2"));

        let rc = render_projected(&c, 10_000, &engine(), 4, &handles, 0, false);
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
        assert!(!rendered.contains("[collapsed:"));
    }

    // ── P1-F: state-turn recency footer ───────────────────────────────────

    #[test]
    fn state_turn_footer_leads_with_next_step_not_bare_goal() {
        let mut c = ctx();
        c.task_state = TaskState {
            goal: "ship the cache work".to_string(),
            plan: vec![PlanStep {
                label: "do E".to_string(),
                done: false,
            }],
            current_step: Some(0),
            ..Default::default()
        };
        c.task_state.record_directive("don't break ABI");
        let rc = render(&c, 100_000, &engine(), 4);
        let text = rc
            .state_turn
            .unwrap()
            .content
            .as_text()
            .unwrap()
            .to_string();

        // The full TASK STATE block still LEADS (primacy) — goal-adherence preserved ...
        assert!(text.starts_with("[TASK STATE] goal: ship the cache work"));
        // ... but the peak-attention footer leads with the forward action, not a goal restatement.
        let before_proceed = text
            .rsplit_once("\n\nProceed.")
            .expect("ends with Proceed")
            .0;
        let last_block = before_proceed.rsplit("\n\n").next().unwrap();
        assert!(
            last_block.starts_with("→ next: step 1 — do E"),
            "got: {last_block}"
        );
        assert!(last_block.contains("must: don't break ABI"));
        // The bare goal must NOT be re-injected at the peak-attention tail (the repetition fuel).
        assert!(
            !last_block.contains("focus: ship the cache work"),
            "got: {last_block}"
        );
    }

    #[test]
    fn footer_falls_back_to_focus_goal_when_nothing_done_yet() {
        // Turn 1: no actions, no plan — the footer surfaces the goal so the model knows the objective.
        let mut c = ctx();
        c.task_state = TaskState {
            goal: "build the thing".to_string(),
            ..Default::default()
        };
        let rc = render(&c, 100_000, &engine(), 4);
        let text = rc
            .state_turn
            .unwrap()
            .content
            .as_text()
            .unwrap()
            .to_string();
        let footer = text
            .rsplit_once("\n\nProceed.")
            .unwrap()
            .0
            .rsplit("\n\n")
            .next()
            .unwrap();
        assert_eq!(footer, "→ focus: build the thing");
    }

    #[test]
    fn footer_shows_recent_actions_and_forward_nudge_without_a_plan() {
        // No curated plan, but real tool activity (2b) → the footer shows motion + a forward nudge,
        // and the goal is NOT restated at the tail.
        let mut c = ctx();
        c.task_state = TaskState {
            goal: "rebuild §4.4 as SVG".to_string(),
            ..Default::default()
        };
        c.task_state.note_actions("module_list");
        c.task_state.note_actions("module_read");
        let rc = render(&c, 100_000, &engine(), 4);
        let footer = rc
            .state_turn
            .unwrap()
            .content
            .as_text()
            .unwrap()
            .rsplit_once("\n\nProceed.")
            .unwrap()
            .0
            .rsplit("\n\n")
            .next()
            .unwrap()
            .to_string();
        assert!(
            footer.contains("did: module_list → module_read"),
            "got: {footer}"
        );
        assert!(footer.contains("next: advance the goal"), "got: {footer}");
        assert!(
            !footer.contains("focus: rebuild §4.4 as SVG"),
            "goal must not lead the footer"
        );
    }

    #[test]
    fn footer_raises_stop_on_repeated_action() {
        // The same action on the last ≥2 turns ⇒ explicit STOP backstop (breaks the read-loop in-band).
        let mut c = ctx();
        c.task_state = TaskState {
            goal: "g".to_string(),
            ..Default::default()
        };
        c.task_state.note_actions("document_read");
        c.task_state.note_actions("document_read");
        c.task_state.note_actions("document_read");
        let rc = render(&c, 100_000, &engine(), 4);
        let footer = rc
            .state_turn
            .unwrap()
            .content
            .as_text()
            .unwrap()
            .rsplit_once("\n\nProceed.")
            .unwrap()
            .0
            .rsplit("\n\n")
            .next()
            .unwrap()
            .to_string();
        assert!(
            footer.contains("STOP: `document_read` repeated 3×"),
            "got: {footer}"
        );
    }

    #[test]
    fn no_salience_footer_without_a_goal() {
        let mut c = ctx();
        c.signals.push("[ROLLBACK] tool failed".to_string());
        let rc = render(&c, 100_000, &engine(), 4);
        let text = rc
            .state_turn
            .unwrap()
            .content
            .as_text()
            .unwrap()
            .to_string();
        assert!(!text.contains("→ focus:"), "no goal ⇒ no footer");
        // signals remain the last content before the anchor.
        assert!(text.contains("[ROLLBACK] tool failed"));
    }

    // ── P0-A: prefix fingerprint (cache-drift instrument) ──────────────────

    #[test]
    fn prefix_fingerprint_is_stable_when_appending_history() {
        let mut c = ctx();
        c.system.push(Message::system("rules"), 5);
        c.knowledge.push(Message::system("skill: debug"), 5);
        c.history.push(Message::user("turn A"), 5);
        c.history.push(Message::assistant("turn B"), 5);
        let fp1 = render(&c, 100_000, &engine(), 4).prefix_fingerprint();

        // Append a new turn — the existing prefix must stay byte-identical.
        c.history.push(Message::user("turn C"), 5);
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
        c.history.push(Message::user("turn A"), 5);
        c.task_state = TaskState {
            goal: "first goal".to_string(),
            ..Default::default()
        };
        let fp1 = render(&c, 100_000, &engine(), 4).prefix_fingerprint();

        c.task_state = TaskState {
            goal: "totally different goal".to_string(),
            ..Default::default()
        };
        c.signals.push("[ROLLBACK] whatever".to_string());
        let fp2 = render(&c, 100_000, &engine(), 4).prefix_fingerprint();

        assert_eq!(
            fp1, fp2,
            "volatile state must not perturb the cacheable prefix"
        );
    }

    #[test]
    fn prefix_fingerprint_detects_system_drift() {
        let mut c = ctx();
        c.system.push(Message::system("rules v1"), 5);
        c.history.push(Message::user("turn A"), 5);
        let fp1 = render(&c, 100_000, &engine(), 4).prefix_fingerprint();

        c.system.messages.clear();
        c.system.push(Message::system("rules v2"), 5);
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
        c.history.push(Message::user("start"), 5);
        let long = "DATA ".repeat(200);
        c.history.push(
            Message::tool(vec![ContentPart::ToolResult {
                call_id: "c1".into(),
                output: long,
                is_error: false,
            }]),
            250,
        );
        c.history.push(Message::user("recent"), 5);

        let resident = render(&c, 100_000, &engine(), 4).prefix_fingerprint();

        // Collapsing the old tool result rewrites that turn in place → the prefix
        // hash at that position changes (the cache-cost of folding, made visible).
        let mut handles = HandleTable::new();
        let mut h = Handle::resident_for(1, HandleKind::ToolResult, 250, "c1");
        h.residency = Residency::Collapsed;
        handles.insert(h);
        let collapsed =
            render_projected(&c, 100_000, &engine(), 4, &handles, 0, false).prefix_fingerprint();

        // turn 0 ("start") is byte-stable; the collapsed tool result at turn 1 drifts.
        assert_eq!(
            collapsed.common_turn_prefix(&resident),
            1,
            "drift begins at the collapsed turn"
        );
        assert!(!collapsed.extends(&resident));
    }

    // ── Method 1: assistant-narration collapse ─────────────────────────────

    fn assistant_with_call(text: &str) -> Message {
        let mut m = Message::assistant(text);
        m.tool_calls = vec![crate::types::message::ToolCall {
            id: "c1".into(),
            name: "module_read".into(),
            arguments: serde_json::json!({}),
        }];
        m
    }

    #[test]
    fn old_assistant_narration_collapses_keeping_tool_calls() {
        let mut c = ctx();
        // Oldest = a long preamble + a tool call; then enough recent turns to push it past the window.
        c.history.push(assistant_with_call(&"好的，我来将 §4.4 的 Mermaid 部署架构图重新构建为 SVG 版本。先找到当前 Mermaid 模块的位置。".repeat(1)), 60);
        c.history.push(
            Message::tool(vec![ContentPart::ToolResult {
                call_id: "c1".into(),
                output: "located".into(),
                is_error: false,
            }]),
            2,
        );
        for i in 0..5 {
            c.history.push(Message::user(format!("recent {i}")), 5);
        }

        // collapse ON (preserve window = 4, so the oldest narration turn is past it)
        let rc = render_projected(&c, 100_000, &engine(), 4, &HandleTable::new(), 0, true);
        let narration = rc
            .turns
            .iter()
            .find(|m| m.content.as_text() == Some(NARRATION_STUB))
            .expect("old narration replaced by stub");
        assert_eq!(
            narration.tool_calls.len(),
            1,
            "tool call (pairing) preserved"
        );
        assert_eq!(narration.tool_calls[0].name, "module_read");
        // No verbatim preamble survives in the rendered prefix.
        assert!(!rc.turns.iter().any(|m| {
            m.content
                .as_text()
                .map(|t| t.contains("先找到当前 Mermaid"))
                .unwrap_or(false)
        }));
        // Original history is untouched (non-destructive projection).
        assert!(
            c.history.messages[0]
                .content
                .as_text()
                .unwrap()
                .contains("先找到当前 Mermaid")
        );

        // collapse OFF → verbatim narration survives.
        let rc_off = render_projected(&c, 100_000, &engine(), 4, &HandleTable::new(), 0, false);
        assert!(rc_off.turns.iter().any(|m| {
            m.content
                .as_text()
                .map(|t| t.contains("先找到当前 Mermaid"))
                .unwrap_or(false)
        }));
    }

    #[test]
    fn recent_assistant_narration_within_window_is_not_collapsed() {
        let mut c = ctx();
        // Only 2 turns, preserve window = 4 → the narration turn is protected → never collapsed.
        c.history.push(
            assistant_with_call(
                &"好的，我来将 §4.4 重新构建为 SVG。先定位模块位置确认范围读取内容。".to_string(),
            ),
            60,
        );
        c.history.push(
            Message::tool(vec![ContentPart::ToolResult {
                call_id: "c1".into(),
                output: "located".into(),
                is_error: false,
            }]),
            2,
        );
        c.history.push(Message::user("ok"), 5);
        let rc = render_projected(&c, 100_000, &engine(), 4, &HandleTable::new(), 0, true);
        assert!(
            rc.turns.iter().any(|m| m
                .content
                .as_text()
                .map(|t| t.contains("先定位模块位置"))
                .unwrap_or(false)),
            "recent narration kept verbatim"
        );
    }

    #[test]
    fn assistant_without_tool_calls_is_never_collapsed() {
        let mut c = ctx();
        // A pure final answer (no tool calls) is substantive — must survive even when old.
        c.history.push(
            Message::assistant("这是给用户的最终结论，包含实质内容，不应被折叠掉以免丢信息。"),
            40,
        );
        for i in 0..5 {
            c.history.push(Message::user(format!("r{i}")), 5);
        }
        let rc = render_projected(&c, 100_000, &engine(), 4, &HandleTable::new(), 0, true);
        assert!(
            rc.turns.iter().any(|m| m
                .content
                .as_text()
                .map(|t| t.contains("最终结论"))
                .unwrap_or(false)),
            "answer-only turns are not narration"
        );
    }

    #[test]
    fn collapsing_narration_drifts_only_that_turn_in_the_cache_prefix() {
        // The cost made visible: collapsing rewrites that one turn in place → the prefix hash drifts
        // at its position (one-time, as it ages past the window), but earlier turns stay reusable.
        let mut c = ctx();
        c.history.push(Message::user("start"), 5);
        c.history.push(assistant_with_call(&"好的，我来将 §4.4 重新构建为 SVG 版本。先找到 Mermaid 模块的确切位置再读取其内容。".to_string()), 60);
        c.history.push(
            Message::tool(vec![ContentPart::ToolResult {
                call_id: "c1".into(),
                output: "located".into(),
                is_error: false,
            }]),
            2,
        );
        for i in 0..4 {
            c.history.push(Message::user(format!("recent {i}")), 5);
        }

        let verbatim = render_projected(&c, 100_000, &engine(), 4, &HandleTable::new(), 0, false)
            .prefix_fingerprint();
        let collapsed = render_projected(&c, 100_000, &engine(), 4, &HandleTable::new(), 0, true)
            .prefix_fingerprint();
        // turn 0 ("start") is byte-stable; drift begins at the collapsed narration turn (index 1).
        assert_eq!(
            collapsed.common_turn_prefix(&verbatim),
            1,
            "only the collapsed turn drifts"
        );
        assert!(!collapsed.extends(&verbatim));
    }

    #[test]
    fn protected_recent_messages_kept_whole_over_budget() {
        let mut c = ctx();
        c.history.push(Message::user("first message"), 5);
        c.history.push(Message::user("a".repeat(1000)), 250);
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
        c.history.push(Message::user("old"), 10);
        c.history.push(Message::assistant("old answer"), 10);
        let mut call = Message::assistant("calling");
        call.tool_calls.push(crate::types::message::ToolCall {
            id: "call-1".into(),
            name: "read".into(),
            arguments: serde_json::json!({}),
        });
        c.history.push(Message::user("question"), 10);
        c.history.push(call, 10);
        c.history.push(
            Message::tool(vec![crate::types::message::ContentPart::ToolResult {
                call_id: "call-1".into(),
                output: "ok".into(),
                is_error: false,
            }]),
            10,
        );
        c.history.push(Message::assistant("answer"), 10);

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
        c.history.push(Message::user("a".repeat(1000)), 250); // oldest, oversized
        c.history.push(Message::user("recent"), 2); // newest, fits
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
        c.history.push(Message::user("x".repeat(120)), 30);
        let state_tokens = engine().count_message(&build_state_turn(&c).expect("state"));

        let rc = render(&c, state_tokens + 5, &engine(), 0);

        assert!(
            rc.turns.is_empty(),
            "history must not consume the state reservation"
        );
        assert!(rc.budget_overflow.is_none());
    }

    #[test]
    fn protected_tail_overflow_is_reported_instead_of_hidden() {
        let mut c = ctx();
        c.history.push(Message::user("x".repeat(400)), 100);

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
        c.system.push(Message::system("x".repeat(400)), 100);

        let rc = render(&c, 10, &engine(), 0);

        let overflow = rc
            .budget_overflow
            .expect("fixed context overflow must be explicit");
        assert_eq!(overflow.kind, ContextBudgetOverflowKind::FixedContext);
        assert_eq!(overflow.required_tokens, 100);
        assert_eq!(overflow.max_tokens, 10);
    }
}
