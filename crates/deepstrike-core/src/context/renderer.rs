use super::partitions::ContextPartitions;
use super::token_engine::ContextTokenEngine;
use crate::mm::handle::{HandleTable, Residency};
use crate::types::message::{Content, ContentPart, Message, Role};
use serde::{Deserialize, Serialize};

/// Structured render output aligned with LLM API slots.
///
/// Slot 1 — system_stable:    Identity (system partition). Anthropic system[0] cache_control.
/// Slot 2 — system_knowledge: Knowledge partition. Anthropic system[1] cache_control.
/// Slot 3 — turns[0]:         State (task_state + signals). Rebuilt every call.
/// Slot 4 — turns[1..N]:      History turns.
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
    /// Turns: [0] = State (task_state + signals), [1..N] = History.
    pub turns: Vec<Message>,
}

fn build_system_stable(partitions: &ContextPartitions) -> String {
    partitions.system.messages
        .iter()
        .filter_map(|m| m.content.as_text())
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn build_system_knowledge(partitions: &ContextPartitions) -> String {
    partitions.knowledge.messages
        .iter()
        .filter_map(|m| m.content.as_text())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Build the State turn (messages[0]): task_state + signals + "Proceed." anchor.
fn build_state_turn(partitions: &ContextPartitions) -> Option<Message> {
    let task = partitions.task_state.format_compact();
    if task.is_empty() && partitions.signals.is_empty() {
        return None;
    }
    let mut parts: Vec<&str> = Vec::new();
    if !task.is_empty() { parts.push(&task); }
    let signals_text = partitions.signals.join("\n");
    if !signals_text.is_empty() { parts.push(&signals_text); }
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
/// only the rendered copy shrinks, so the projection reverses when pressure drops.
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
            ContentPart::ToolResult { call_id, output, is_error }
                if matches!(
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
/// Equivalent to [`render_projected`] with an empty handle table (no Layer-4 projection).
pub fn render(
    partitions: &ContextPartitions,
    budget: u32,
    engine: &ContextTokenEngine,
    preserve_recent_msgs: usize,
) -> RenderedContext {
    render_projected(partitions, budget, engine, preserve_recent_msgs, &HandleTable::new())
}

/// Render with Layer-4 read-time projection driven by `handles`: tool results whose handle is
/// `Collapsed` render as previews (originals untouched), freeing budget for more recent turns.
///
/// Token budget:
///   system_stable + system_knowledge tokens are subtracted first.
///   Remaining budget is allocated to history turns newest-first.
///   The first `preserve_recent_msgs` history messages are always included.
///   Text messages are truncated at the budget boundary; Parts messages are included whole.
pub fn render_projected(
    partitions: &ContextPartitions,
    budget: u32,
    engine: &ContextTokenEngine,
    preserve_recent_msgs: usize,
    handles: &HandleTable,
) -> RenderedContext {
    let system_stable = build_system_stable(partitions);
    let system_knowledge = build_system_knowledge(partitions);
    let system_text = [system_stable.as_str(), system_knowledge.as_str()]
        .iter()
        .filter(|s| !s.is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join("\n\n");

    let system_tokens = engine.count(&system_text).min(budget);
    let mut remaining = budget.saturating_sub(system_tokens);

    // Fill history newest-first within remaining budget. Layer-4 projection is applied per
    // message: a collapsed tool result renders as a preview and is costed at its reduced size.
    let mut kept_rev: Vec<Message> = Vec::new();
    for msg in partitions.history.messages.iter().rev() {
        // `projected` is `Some` only when read-time projection shrank the message.
        let projected = project_message(msg, handles);
        let effective = projected.as_ref().unwrap_or(msg);
        let tokens = match &projected {
            Some(p) => engine.count_message(p),
            None => msg.token_count.unwrap_or_else(|| engine.count_message(msg)),
        };
        if tokens == 0 { continue; }

        let is_protected = kept_rev.len() < preserve_recent_msgs;
        if is_protected {
            kept_rev.push(effective.clone());
            remaining = remaining.saturating_sub(tokens);
            continue;
        }

        if tokens <= remaining {
            kept_rev.push(effective.clone());
            remaining = remaining.saturating_sub(tokens);
        } else if remaining > 0 {
            match &effective.content {
                Content::Text(_) => kept_rev.push(engine.truncate_message(effective, remaining)),
                Content::Parts(_) => kept_rev.push(effective.clone()),
            }
            break;
        } else {
            break;
        }
    }

    kept_rev.reverse();
    let mut turns = kept_rev;
    normalize_turn_prefix(&mut turns);

    // Prepend the State turn (task_state + signals) as turns[0].
    if let Some(state_turn) = build_state_turn(partitions) {
        turns.insert(0, state_turn);
    }

    RenderedContext { system_text, system_stable, system_knowledge, turns }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::config::ContextConfig;
    use crate::context::partitions::ContextPartitions;
    use crate::context::task_state::TaskState;
    use crate::context::token_engine::ContextTokenEngine;
    use crate::types::message::{Message, Role};

    fn engine() -> ContextTokenEngine { ContextTokenEngine::char_approx() }
    fn ctx() -> ContextPartitions { ContextPartitions::new(&ContextConfig::default()) }

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
    fn task_state_appears_in_turns_first_user() {
        let mut c = ctx();
        c.task_state = TaskState { goal: "find the bug".to_string(), ..Default::default() };
        let rc = render(&c, 10_000, &engine(), 4);
        assert!(!rc.system_text.contains("[TASK STATE]"), "task_state must not be in system_text");
        let first = rc.turns.first().expect("should have a state turn");
        assert_eq!(first.role, Role::User);
        assert!(first.content.as_text().unwrap().contains("[TASK STATE] goal: find the bug"));
    }

    #[test]
    fn signals_appear_in_state_turn() {
        let mut c = ctx();
        c.task_state = TaskState { goal: "g".to_string(), ..Default::default() };
        c.signals.push("[ROLLBACK] tool failed".to_string());
        let rc = render(&c, 10_000, &engine(), 4);
        let first = rc.turns.first().unwrap();
        assert!(first.content.as_text().unwrap().contains("[ROLLBACK] tool failed"));
    }

    #[test]
    fn empty_task_state_no_state_turn() {
        let c = ctx();
        let rc = render(&c, 10_000, &engine(), 4);
        // No state turn when task_state is empty and no signals
        assert!(rc.turns.is_empty());
    }

    #[test]
    fn history_follows_state_turn() {
        let mut c = ctx();
        c.task_state = TaskState { goal: "g".to_string(), ..Default::default() };
        c.history.push(Message::user("step 1"), 5);
        c.history.push(Message::assistant("done"), 5);
        let rc = render(&c, 10_000, &engine(), 4);
        assert_eq!(rc.turns[0].role, Role::User); // state turn
        assert!(rc.turns[0].content.as_text().unwrap().contains("[TASK STATE]"));
        assert_eq!(rc.turns[1].role, Role::User);
        assert_eq!(rc.turns[2].role, Role::Assistant);
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

        let rc = render_projected(&c, 10_000, &engine(), 4, &handles);
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

        let rc = render_projected(&c, 10_000, &engine(), 4, &handles);
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

    #[test]
    fn text_truncated_when_budget_exhausted() {
        let mut c = ctx();
        c.history.push(Message::user("first message"), 5);
        c.history.push(Message::user("a".repeat(1000)), 250);
        let rc = render(&c, 10, &engine(), 4);
        assert!(rc.turns.iter().any(|m| {
            m.content.as_text().map(|t| t.contains("first message")).unwrap_or(false)
        }));
    }
}
