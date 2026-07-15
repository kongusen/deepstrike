use crate::types::message::{Content, ContentPart, Message, Role};
use std::collections::BTreeSet;
use std::ops::Range;

/// Partition history into indivisible conversational transactions.
///
/// A new user turn starts a unit only when no tool call is awaiting a result. This keeps an
/// assistant call, every correlated result, and the trailing answer on the same side of every
/// compression/render boundary. Malformed recovery input is kept together until its open calls
/// close, which is safer than manufacturing an orphaned result.
pub fn unit_boundaries(messages: &[Message]) -> Vec<Range<usize>> {
    if messages.is_empty() {
        return Vec::new();
    }

    let mut boundaries = Vec::new();
    let mut start = 0usize;
    let mut open_calls = BTreeSet::new();
    let mut has_assistant = false;
    let mut has_tool_transaction = false;
    let mut trailing_answer_consumed = false;

    for (index, message) in messages.iter().enumerate() {
        let is_trailing_answer = has_tool_transaction
            && !trailing_answer_consumed
            && open_calls.is_empty()
            && message.role == Role::Assistant
            && message.tool_calls.is_empty();
        let starts_new_user_unit = message.role == Role::User && open_calls.is_empty();
        let starts_new_assistant_unit = message.role == Role::Assistant
            && open_calls.is_empty()
            && !is_trailing_answer
            && has_assistant;
        let starts_new_orphan_tool_unit =
            message.role == Role::Tool && open_calls.is_empty() && index > start;
        if index > start
            && (starts_new_user_unit || starts_new_assistant_unit || starts_new_orphan_tool_unit)
        {
            boundaries.push(start..index);
            start = index;
            has_assistant = false;
            has_tool_transaction = false;
            trailing_answer_consumed = false;
        } else if is_trailing_answer {
            trailing_answer_consumed = true;
        }

        if message.role == Role::Assistant {
            has_assistant = true;
        }
        for call in &message.tool_calls {
            open_calls.insert(call.id.to_string());
            has_tool_transaction = true;
        }
        if let Content::Parts(parts) = &message.content {
            for part in parts {
                if let ContentPart::ToolResult { call_id, .. } = part {
                    open_calls.remove(call_id.as_str());
                }
            }
        }
    }

    boundaries.push(start..messages.len());
    boundaries
}

/// Rust-side equivalent of the strict provider replay pairing invariant. This is intentionally
/// small and deterministic so compression and rendering tests can prove they never manufacture an
/// orphan or leave a call unanswered when their input was valid.
pub(crate) fn strict_tool_pairing_is_valid(messages: &[Message]) -> bool {
    let mut pending: Option<BTreeSet<String>> = None;
    let mut completed = BTreeSet::new();

    let pending_complete = |pending: &Option<BTreeSet<String>>, completed: &BTreeSet<String>| {
        pending
            .as_ref()
            .is_none_or(|ids| ids.iter().all(|id| completed.contains(id)))
    };

    for message in messages {
        if message.role == Role::Assistant {
            if !pending_complete(&pending, &completed) {
                return false;
            }
            pending = (!message.tool_calls.is_empty()).then(|| {
                message
                    .tool_calls
                    .iter()
                    .map(|call| call.id.to_string())
                    .collect()
            });
            completed.clear();
            continue;
        }

        if message.role != Role::Tool {
            if !pending_complete(&pending, &completed) {
                return false;
            }
            pending = None;
            completed.clear();
            continue;
        }

        if let Content::Parts(parts) = &message.content {
            for part in parts {
                if let ContentPart::ToolResult { call_id, .. } = part {
                    if !pending
                        .as_ref()
                        .is_some_and(|ids| ids.contains(call_id.as_str()))
                        || !completed.insert(call_id.to_string())
                    {
                        return false;
                    }
                }
            }
        }
    }

    pending_complete(&pending, &completed)
}

#[cfg(test)]
mod tests {
    use crate::types::message::{ContentPart, Message, ToolCall};

    fn assistant_call(id: &str) -> Message {
        let mut message = Message::assistant("calling");
        message.tool_calls.push(ToolCall {
            id: id.into(),
            name: "read".into(),
            arguments: serde_json::json!({}),
        });
        message
    }

    fn tool_result(id: &str) -> Message {
        Message::tool(vec![ContentPart::ToolResult {
            call_id: id.into(),
            output: "ok".into(),
            is_error: false,
        }])
    }

    #[test]
    fn groups_tool_transaction_and_trailing_answer_as_one_unit() {
        let messages = vec![
            Message::user("question"),
            assistant_call("call-1"),
            tool_result("call-1"),
            Message::assistant("answer"),
            Message::user("next"),
            Message::assistant("done"),
        ];

        assert_eq!(super::unit_boundaries(&messages), vec![0..4, 4..6]);
    }

    #[test]
    fn iterative_tool_loop_is_partitioned_without_requiring_new_user_messages() {
        let messages = vec![
            Message::user("do the task"),
            assistant_call("call-1"),
            tool_result("call-1"),
            assistant_call("call-2"),
            tool_result("call-2"),
            assistant_call("call-3"),
            tool_result("call-3"),
            Message::assistant("done"),
        ];

        assert_eq!(super::unit_boundaries(&messages), vec![0..3, 3..5, 5..8]);
        for unit in super::unit_boundaries(&messages) {
            assert!(super::strict_tool_pairing_is_valid(&messages[unit]));
        }
    }

    #[test]
    fn does_not_split_at_user_boundary_while_tool_results_are_missing() {
        let messages = vec![
            assistant_call("call-1"),
            Message::user("provider recovery"),
            tool_result("call-1"),
            Message::user("next"),
        ];

        assert_eq!(super::unit_boundaries(&messages), vec![0..3, 3..4]);
    }

    #[test]
    fn empty_history_has_no_units() {
        assert!(super::unit_boundaries(&[]).is_empty());
    }

    #[test]
    fn standalone_tool_messages_are_independent_units() {
        let messages = vec![tool_result("orphan-1"), tool_result("orphan-2")];
        assert_eq!(super::unit_boundaries(&messages), vec![0..1, 1..2]);
    }

    #[test]
    fn strict_pairing_validator_matches_provider_contract() {
        let valid = vec![assistant_call("call-1"), tool_result("call-1")];
        assert!(super::strict_tool_pairing_is_valid(&valid));
        assert!(!super::strict_tool_pairing_is_valid(&valid[..1]));
        assert!(!super::strict_tool_pairing_is_valid(&[tool_result(
            "orphan"
        )]));
    }
}
