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
                    format!("{}...", &t[..200])
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
