use crate::context::pressure::PressureAction;
use crate::types::message::{Message, Role};

/// Deterministic rule-based summariser — no LLM required. The compression
/// pipeline is its only consumer; richer (LLM) summaries are an SDK concern.
pub struct RuleSummarizer;

impl RuleSummarizer {
    /// Produce a summary of `messages` (the `max_tokens` budget is currently advisory).
    pub fn summarize(&self, messages: &[Message], action: PressureAction, _max_tokens: u32) -> String {
        let n = messages.len();
        let tokens: u32 = messages.iter().map(|m| m.token_count.unwrap_or(0)).sum();
        let mut tool_names: Vec<String> = messages
            .iter()
            .flat_map(|m| m.tool_calls.iter().map(|tc| tc.name.to_string()))
            .collect();
        tool_names.sort();
        tool_names.dedup();

        let last_assistant = messages
            .iter()
            .rev()
            .find(|m| m.role == Role::Assistant)
            .and_then(|m| m.content.as_text())
            .map(|t| {
                if t.len() > 200 {
                    // Cut on a char boundary — a raw `&t[..200]` panics (and aborts the
                    // whole process) when byte 200 lands inside a multi-byte scalar, e.g. CJK.
                    format!(
                        "{}...",
                        crate::context::text::truncate_bytes_at_char_boundary(t, 200)
                    )
                } else {
                    t.to_string()
                }
            })
            .unwrap_or_default();

        let action_str = action.label();

        let mut s =
            format!("[Compressed: {action_str}]\n{n} messages / {tokens} tokens archived\n");
        if !tool_names.is_empty() {
            s.push_str(&format!("tools used: {}\n", tool_names.join(", ")));
        }
        if !last_assistant.is_empty() {
            s.push_str(&format!("last assistant output: {last_assistant}"));
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::message::Content;

    /// Regression: a long CJK assistant message whose 200th byte falls inside a
    /// multi-byte scalar must not panic (previously aborted the whole process).
    #[test]
    fn summarize_does_not_panic_on_cjk_boundary() {
        // 100 × "规范" = 200 chars / 600 bytes; byte 200 is inside a '规' (bytes 198..201).
        let long_cjk = "规范".repeat(100);
        assert!(!long_cjk.is_char_boundary(200));

        let msg = Message {
            role: Role::Assistant,
            content: Content::Text(long_cjk),
            tool_calls: vec![],
            token_count: None,
        };
        let out = RuleSummarizer.summarize(&[msg], PressureAction::AutoCompact, 1000);
        assert!(out.contains("规范"));
        assert!(out.contains("..."));
    }
}
