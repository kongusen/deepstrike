use crate::context::pressure::PressureAction;
use crate::context::token_engine::ContextTokenEngine;
use crate::types::message::{Content, ContentPart, Message};

/// Deterministic six-slot summariser used before archived units page out.
pub struct RuleSummarizer;

/// Items rendered per slot before an honest `(+N more)` line. Bounds each digest so the
/// task-state compression history can afford to keep MANY digests visible instead of a few
/// bloated ones.
const SLOT_ITEM_CAP: usize = 6;

impl RuleSummarizer {
    /// Produce a structured summary whose char-approx token count never exceeds
    /// `max_tokens`. Slot order is the deterministic truncation priority.
    pub fn summarize(
        &self,
        messages: &[Message],
        action: PressureAction,
        max_tokens: u32,
    ) -> String {
        if max_tokens == 0 {
            return String::new();
        }
        let engine = ContextTokenEngine::char_approx();
        let archived_tokens = messages
            .iter()
            .map(|message| {
                message
                    .token_count
                    .unwrap_or_else(|| engine.count_message(message))
            })
            .sum::<u32>();
        let mut slots = SummarySlots::default();
        for message in messages {
            for call in &message.tool_calls {
                push_unique(
                    &mut slots.artifacts,
                    format!("tool {} args {}", call.name, call.arguments),
                );
            }
            match &message.content {
                Content::Text(text) => classify_text(text, &mut slots),
                Content::Parts(parts) => {
                    for part in parts {
                        match part {
                            ContentPart::Text { text } => classify_text(text, &mut slots),
                            ContentPart::ToolResult {
                                call_id,
                                output,
                                is_error,
                            } => {
                                if *is_error {
                                    push_unique(
                                        &mut slots.failures,
                                        format!("tool {call_id}: {}", compact(output, 240)),
                                    );
                                }
                                classify_text(output, &mut slots);
                            }
                            ContentPart::Image { url, .. } => {
                                if let Some(url) = url {
                                    push_unique(&mut slots.artifacts, url.clone());
                                }
                            }
                            ContentPart::Audio { .. } => {}
                        }
                    }
                }
            }
        }

        let mut output = String::new();
        push_line(
            &mut output,
            &format!("[Compressed: {}]", action.label()),
            max_tokens,
            &engine,
        );
        push_line(
            &mut output,
            &format!(
                "archived_messages: {}; archived_tokens: {archived_tokens}",
                messages.len()
            ),
            max_tokens,
            &engine,
        );
        for (name, values) in [
            ("constraints", slots.constraints),
            ("decisions", slots.decisions),
            ("artifacts", slots.artifacts),
            ("open_questions", slots.open_questions),
            ("failures", slots.failures),
            ("next_actions", slots.next_actions),
        ] {
            if !push_line(&mut output, &format!("{name}:"), max_tokens, &engine) {
                break;
            }
            if values.is_empty() {
                push_line(&mut output, "- none", max_tokens, &engine);
            } else {
                for value in values.iter().take(SLOT_ITEM_CAP) {
                    push_line(
                        &mut output,
                        &format!("- {}", compact(value, 240)),
                        max_tokens,
                        &engine,
                    );
                }
                if values.len() > SLOT_ITEM_CAP {
                    push_line(
                        &mut output,
                        &format!("- (+{} more)", values.len() - SLOT_ITEM_CAP),
                        max_tokens,
                        &engine,
                    );
                }
            }
        }

        if engine.count(&output) > max_tokens {
            engine.truncate(&output, max_tokens).to_string()
        } else {
            output
        }
    }
}

#[derive(Default)]
struct SummarySlots {
    constraints: Vec<String>,
    decisions: Vec<String>,
    artifacts: Vec<String>,
    open_questions: Vec<String>,
    failures: Vec<String>,
    next_actions: Vec<String>,
}

fn classify_text(text: &str, slots: &mut SummarySlots) {
    for statement in statements(text) {
        if is_diff_noise(&statement) {
            continue;
        }
        let folded = statement.to_lowercase();
        if contains_any(
            &folded,
            &[
                "constraint",
                "must",
                "required",
                "do not",
                "should",
                "约束",
                "必须",
                "不得",
                "应当",
            ],
        ) {
            push_unique(&mut slots.constraints, statement.clone());
        }
        if contains_any(
            &folded,
            &["decision", "decided", "selected", "choose", "决定", "选择"],
        ) {
            push_unique(&mut slots.decisions, statement.clone());
        }
        if contains_any(
            &folded,
            &[
                "error",
                "failed",
                "failure",
                "exception",
                "timeout",
                "错误",
                "失败",
                "异常",
                "超时",
            ],
        ) {
            push_unique(&mut slots.failures, statement.clone());
        }
        if statement.contains('?')
            || statement.contains('？')
            || contains_any(
                &folded,
                &["open question", "unresolved", "unknown", "待确认", "未解决"],
            )
        {
            push_unique(&mut slots.open_questions, statement.clone());
        }
        if contains_any(
            &folded,
            &[
                "next",
                "todo",
                "then",
                "follow up",
                "下一步",
                "待办",
                "随后",
            ],
        ) {
            push_unique(&mut slots.next_actions, statement.clone());
        }
        if contains_any(&folded, &["artifact", "file", "output", "产物", "文件"])
            || statement
                .split_whitespace()
                .any(|word| word.contains('/') || word.contains("://"))
        {
            push_unique(&mut slots.artifacts, statement);
        }
    }
}

fn statements(text: &str) -> Vec<String> {
    let mut output = Vec::new();
    let mut current = String::new();
    let mut chars = text.chars().peekable();
    while let Some(character) = chars.next() {
        let boundary = match character {
            '\n' | '!' | '?' | ';' | '。' | '！' | '？' | '；' => true,
            // A '.' splits only at a sentence boundary (followed by whitespace or end of
            // text). Splitting inside paths/versions shredded `src/auth.js` into useless
            // `js b/src/auth` fragments that polluted every digest.
            '.' => chars.peek().is_none_or(|next| next.is_whitespace()),
            _ => false,
        };
        if boundary {
            flush_statement(&mut current, &mut output);
        } else {
            current.push(character);
        }
    }
    flush_statement(&mut current, &mut output);
    output
}

fn flush_statement(current: &mut String, output: &mut Vec<String>) {
    let statement = current.trim();
    if !statement.is_empty() {
        output.push(compact(statement, 240));
    }
    current.clear();
}

/// Structural diff/patch header lines carry no summarizable content — dropping them keeps
/// digests dense so more real process state survives a given summary budget.
fn is_diff_noise(statement: &str) -> bool {
    let trimmed = statement.trim_start();
    trimmed.starts_with("diff --git")
        || trimmed.starts_with("+++")
        || trimmed.starts_with("---")
        || trimmed.starts_with("@@")
        || trimmed.starts_with("index ")
}

fn contains_any(text: &str, markers: &[&str]) -> bool {
    markers.iter().any(|marker| text.contains(marker))
}

fn push_unique(values: &mut Vec<String>, value: String) {
    if !value.is_empty() && !values.contains(&value) {
        values.push(value);
    }
}

fn push_line(
    output: &mut String,
    line: &str,
    max_tokens: u32,
    engine: &ContextTokenEngine,
) -> bool {
    let candidate = if output.is_empty() {
        line.to_string()
    } else {
        format!("{output}\n{line}")
    };
    if engine.count(&candidate) > max_tokens {
        return false;
    }
    *output = candidate;
    true
}

fn compact(text: &str, max_chars: usize) -> String {
    let mut output = text.chars().take(max_chars).collect::<String>();
    if text.chars().count() > max_chars {
        output.push('…');
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::message::{ContentPart, ToolCall};

    #[test]
    fn summarize_does_not_panic_on_cjk_boundary() {
        let long_cjk = "规范".repeat(100);
        assert!(!long_cjk.is_char_boundary(200));
        let msg = Message::assistant(format!("必须遵守约束：{long_cjk}"));
        let out = RuleSummarizer.summarize(&[msg], PressureAction::AutoCompact, 1_000);
        assert!(out.contains("规范"));
        assert!(out.contains("constraints:"));
    }

    #[test]
    fn emits_six_structured_slots_from_rules_tools_and_errors() {
        let mut call = Message::assistant(
            "DECISION: choose parser B. Must preserve schema. Open question: retry limit? Next: run tests.",
        );
        call.tool_calls.push(ToolCall {
            id: "call-1".into(),
            name: "write_file".into(),
            arguments: serde_json::json!({"path": "/work/report.json"}),
        });
        let result = Message::tool(vec![ContentPart::ToolResult {
            call_id: "call-1".into(),
            output: "ERROR: write failed; artifact /work/report.json".into(),
            is_error: true,
        }]);
        let out = RuleSummarizer.summarize(&[call, result], PressureAction::ContextCollapse, 1_000);
        for slot in [
            "constraints:",
            "decisions:",
            "artifacts:",
            "open_questions:",
            "failures:",
            "next_actions:",
        ] {
            assert!(out.contains(slot), "missing {slot}: {out}");
        }
        assert!(out.contains("write_file"));
        assert!(out.contains("write failed"));
    }

    #[test]
    fn max_tokens_is_a_real_hard_upper_bound() {
        let message = Message::assistant(
            "DECISION: keep this. Must preserve that. Next: run many tests. ERROR: prior attempt failed."
                .repeat(20),
        );
        for max_tokens in [1, 4, 8, 16, 32] {
            let out = RuleSummarizer.summarize(
                std::slice::from_ref(&message),
                PressureAction::AutoCompact,
                max_tokens,
            );
            assert!(
                ContextTokenEngine::char_approx().count(&out) <= max_tokens,
                "max={max_tokens}, output={out:?}"
            );
        }
        assert_eq!(
            RuleSummarizer.summarize(&[message], PressureAction::AutoCompact, 0),
            ""
        );
    }
}
