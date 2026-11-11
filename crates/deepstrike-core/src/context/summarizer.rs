use std::sync::OnceLock;

use crate::context::measurement::TokenMeasurement;
use crate::context::pressure::PressureAction;
use crate::context::token_engine::ContextTokenEngine;
use crate::types::message::{Content, ContentPart, CoreMessage};

/// Deterministic six-slot summariser used before archived units page out.
pub struct RuleSummarizer;

/// Items rendered per slot before an honest `(+N more)` line. Bounds each digest so the
/// task-state compression history can afford to keep MANY digests visible instead of a few
/// bloated ones.
const SLOT_ITEM_CAP: usize = 6;

static SUMMARY_ENGINE: OnceLock<ContextTokenEngine> = OnceLock::new();

impl RuleSummarizer {
    /// Produce a structured summary whose fallback-estimated token count never exceeds
    /// `max_tokens`. Slot order is the deterministic truncation priority.
    pub fn summarize(
        &self,
        messages: &[CoreMessage],
        measurements: &[TokenMeasurement],
        action: PressureAction,
        max_tokens: u32,
    ) -> String {
        if max_tokens == 0 {
            return String::new();
        }
        // Keep the production BPE semantics without rebuilding the cl100k tables for every
        // compression. ContextTokenEngine is Arc-backed, so one process-wide instance is safe.
        let engine = SUMMARY_ENGINE.get_or_init(ContextTokenEngine::fallback_estimator);
        let archived_tokens = messages
            .iter()
            .enumerate()
            .map(|(index, message)| {
                measurements
                    .get(index)
                    .map(|measurement| measurement.tokens)
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
                                ..
                            } => {
                                if *is_error {
                                    push_unique(
                                        &mut slots.failures,
                                        format!("tool {call_id}: {}", compact(output, 240)),
                                    );
                                }
                                classify_text(output, &mut slots);
                            }
                            ContentPart::Image { source, .. } => {
                                if let crate::types::durable_content::DurableSource::Url { url } =
                                    source
                                {
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
            engine,
        );
        push_line(
            &mut output,
            &format!(
                "archived_messages: {}; archived_tokens: {archived_tokens}",
                messages.len()
            ),
            max_tokens,
            engine,
        );
        for (name, values) in [
            ("constraints", slots.constraints),
            ("decisions", slots.decisions),
            ("artifacts", slots.artifacts),
            ("open_questions", slots.open_questions),
            ("failures", slots.failures),
            ("next_actions", slots.next_actions),
        ] {
            if !push_line(&mut output, &format!("{name}:"), max_tokens, engine) {
                break;
            }
            if values.is_empty() {
                push_line(&mut output, "- none", max_tokens, engine);
            } else {
                for value in values.iter().take(SLOT_ITEM_CAP) {
                    push_line(
                        &mut output,
                        &format!("- {}", compact(value, 240)),
                        max_tokens,
                        engine,
                    );
                }
                if values.len() > SLOT_ITEM_CAP {
                    push_line(
                        &mut output,
                        &format!("- (+{} more)", values.len() - SLOT_ITEM_CAP),
                        max_tokens,
                        engine,
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

/// Plan task 8.4 · the most a model summary may take. It renders in the task state on every request,
/// so it is bounded the same way the rule summary's slots are.
pub const MODEL_SUMMARY_MAX_TOKENS: u32 = 800;

/// The most of the archived range a summary request carries. The newest messages are kept: a
/// running tally lives in the latest narration, and the previous summary covers what came before.
pub const SUMMARY_TRANSCRIPT_MAX_TOKENS: u32 = 16_000;

/// Characters kept of each tool result in a summary transcript: the model's own notes carry the
/// state of the work; a raw tool output is evidence it can read again.
const TRANSCRIPT_TOOL_RESULT_CHARS: usize = 1_500;

const SUMMARY_INSTRUCTIONS: &str = "You compress the earlier part of an AI agent's working session. \
The agent will continue from your summary alone: the messages below leave its context for good. \
Write a plain-text summary that keeps, exactly as stated: the task's rules and constraints; every \
running count, total or tally with what it covers so far (for example which files or pages are \
done); numbers, identifiers and file names the work depends on; decisions made; failures and what \
was tried; and what remains to be done next. If a previous summary is given, carry its facts \
forward and update its counts with the new messages; never restart a count. Do not add commentary, \
advice or facts that are not in the messages.";

/// What the host is asked to summarize, built by the kernel so every SDK sends the same request.
#[derive(Debug, Clone, PartialEq)]
pub struct SummaryRequest {
    /// The compaction tier label (`context_collapse` / `auto_compact`).
    pub action: &'static str,
    pub instructions: String,
    pub transcript: String,
    pub max_tokens: u32,
    pub archived_count: u32,
    /// The rule summary in place now.
    pub fallback: String,
}

impl SummaryRequest {
    /// The request for the range `archived` a `action` compaction just took out, given the summary
    /// that covered the range before it (`previous`) and the rule summary now in place.
    pub fn build(
        archived: &[CoreMessage],
        previous: Option<&str>,
        fallback: &str,
        action: PressureAction,
        engine: &ContextTokenEngine,
    ) -> Self {
        let lines: Vec<String> = archived.iter().map(transcript_line).collect();
        // Newest first until the budget is spent, then back into order.
        let mut kept = Vec::new();
        let mut used = 0u32;
        for line in lines.iter().rev() {
            let tokens = engine.count(line);
            if used.saturating_add(tokens) > SUMMARY_TRANSCRIPT_MAX_TOKENS && !kept.is_empty() {
                break;
            }
            used = used.saturating_add(tokens);
            kept.push(line.as_str());
        }
        kept.reverse();
        let mut transcript = String::new();
        if let Some(previous) = previous.filter(|text| !text.trim().is_empty()) {
            transcript.push_str("Previous summary (covers everything before these messages):\n");
            transcript.push_str(previous.trim());
            transcript.push_str("\n\n");
        }
        let omitted = lines.len() - kept.len();
        transcript.push_str("Messages to summarize, oldest first:\n");
        if omitted > 0 {
            transcript.push_str(&format!("({omitted} earlier messages omitted)\n"));
        }
        transcript.push_str(&kept.join("\n"));
        Self {
            action: action.label(),
            instructions: format!(
                "{SUMMARY_INSTRUCTIONS} Stay under {MODEL_SUMMARY_MAX_TOKENS} tokens."
            ),
            transcript,
            max_tokens: MODEL_SUMMARY_MAX_TOKENS,
            archived_count: archived.len() as u32,
            fallback: fallback.to_string(),
        }
    }
}

/// One message of a summary transcript: its role, its text, its tool calls, and the head of each
/// tool result.
fn transcript_line(message: &CoreMessage) -> String {
    let role = match message.role {
        crate::types::message::Role::System => "system",
        crate::types::message::Role::User => "user",
        crate::types::message::Role::Assistant => "assistant",
        crate::types::message::Role::Tool => "tool",
    };
    let mut parts: Vec<String> = Vec::new();
    match &message.content {
        Content::Text(text) if !text.trim().is_empty() => parts.push(text.trim().to_string()),
        Content::Text(_) => {}
        Content::Parts(items) => {
            for part in items {
                match part {
                    ContentPart::Text { text } if !text.trim().is_empty() => {
                        parts.push(text.trim().to_string())
                    }
                    ContentPart::ToolResult {
                        call_id,
                        output,
                        is_error,
                        ..
                    } => parts.push(format!(
                        "[{} {call_id}] {}",
                        if *is_error { "error" } else { "result" },
                        compact(output, TRANSCRIPT_TOOL_RESULT_CHARS)
                    )),
                    _ => {}
                }
            }
        }
    }
    for call in &message.tool_calls {
        parts.push(format!("-> {}({})", call.name, call.arguments));
    }
    format!("{role}: {}", parts.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::message::{ContentPart, ToolCall};

    #[test]
    fn summarize_does_not_panic_on_cjk_boundary() {
        let long_cjk = "规范".repeat(100);
        assert!(!long_cjk.is_char_boundary(200));
        let msg = CoreMessage::assistant(format!("必须遵守约束：{long_cjk}"));
        let out = RuleSummarizer.summarize(&[msg], &[], PressureAction::AutoCompact, 1_000);
        assert!(out.contains("规范"));
        assert!(out.contains("constraints:"));
    }

    #[test]
    fn emits_six_structured_slots_from_rules_tools_and_errors() {
        let mut call = CoreMessage::assistant(
            "DECISION: choose parser B. Must preserve schema. Open question: retry limit? Next: run tests.",
        );
        call.tool_calls.push(ToolCall {
            id: "call-1".into(),
            name: "write_file".into(),
            arguments: serde_json::json!({"path": "/work/report.json"}),
        });
        let result = CoreMessage::tool(vec![ContentPart::ToolResult {
            call_id: "call-1".into(),
            output: "ERROR: write failed; artifact /work/report.json".into(),
            is_error: true,
            durable_content: None,
        }]);
        let out =
            RuleSummarizer.summarize(&[call, result], &[], PressureAction::ContextCollapse, 1_000);
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
        let message = CoreMessage::assistant(
            "DECISION: keep this. Must preserve that. Next: run many tests. ERROR: prior attempt failed."
                .repeat(20),
        );
        for max_tokens in [1, 4, 8, 16, 32] {
            let out = RuleSummarizer.summarize(
                std::slice::from_ref(&message),
                &[],
                PressureAction::AutoCompact,
                max_tokens,
            );
            assert!(
                ContextTokenEngine::char_approx().count(&out) <= max_tokens,
                "max={max_tokens}, output={out:?}"
            );
        }
        assert_eq!(
            RuleSummarizer.summarize(&[message], &[], PressureAction::AutoCompact, 0),
            ""
        );
    }

    #[test]
    fn summary_request_keeps_the_newest_messages_and_the_previous_summary() {
        let engine = ContextTokenEngine::char_approx();
        let mut messages: Vec<CoreMessage> = (0..400)
            .map(|n| CoreMessage::assistant(format!("note {n}: {}", "x".repeat(400))))
            .collect();
        messages.push(CoreMessage::assistant("7 matches so far, through svc-12"));
        let request = SummaryRequest::build(
            &messages,
            Some("3 matches in svc-01..svc-05"),
            "[Compressed: auto_compact]",
            PressureAction::AutoCompact,
            &engine,
        );
        assert_eq!(request.action, "auto_compact");
        assert_eq!(request.archived_count, 401);
        assert_eq!(request.max_tokens, MODEL_SUMMARY_MAX_TOKENS);
        assert!(request.transcript.starts_with("Previous summary"));
        assert!(request.transcript.contains("3 matches in svc-01..svc-05"));
        assert!(
            request
                .transcript
                .ends_with("assistant: 7 matches so far, through svc-12")
        );
        assert!(request.transcript.contains("earlier messages omitted"));
        assert!(!request.transcript.contains("note 0:"));
        assert!(engine.count(&request.transcript) <= SUMMARY_TRANSCRIPT_MAX_TOKENS + 200);
    }

    #[test]
    fn summary_transcript_shows_calls_and_cuts_tool_results() {
        let mut call = CoreMessage::assistant("reading the next log");
        call.tool_calls.push(ToolCall {
            id: "call-1".into(),
            name: "read_file".into(),
            arguments: serde_json::json!({"path": "logs/svc-01.log"}),
        });
        let result = CoreMessage::tool(vec![ContentPart::ToolResult {
            call_id: "call-1".into(),
            output: "y".repeat(5_000),
            is_error: false,
            durable_content: None,
        }]);
        let request = SummaryRequest::build(
            &[call, result],
            None,
            "",
            PressureAction::ContextCollapse,
            &ContextTokenEngine::char_approx(),
        );
        assert!(!request.transcript.contains("Previous summary"));
        assert!(request.transcript.contains("-> read_file("));
        assert!(request.transcript.contains("[result call-1] "));
        assert!(!request.transcript.contains(&"y".repeat(1_600)));
    }
}
