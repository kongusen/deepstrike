/// LLM-powered memory synthesis — the "dreaming" half of the idle pipeline.
///
/// The kernel is responsible for:
///   1. Assembling a prompt from compressed session traces + rule-based seed insights
///   2. Parsing the LLM's JSON response back into `TraceInsight` objects
///
/// The SDK is responsible for the actual LLM call between steps 1 and 2.
/// This keeps the kernel pure-computation while enabling intelligent synthesis.
use crate::memory::trace_analyzer::{InsightKind, TraceInsight};
use crate::types::message::{Content, Message, Role};

#[derive(Debug, Clone)]
pub struct SynthesisPolicy {
    /// Max chars of session content included in the prompt. Default: 8_000.
    pub max_session_chars: usize,
    /// Max number of insights to request from the LLM. Default: 10.
    pub max_insights: usize,
    /// Prepend rule-based seed insights so the LLM can build on them. Default: true.
    pub include_seed_insights: bool,
}

impl Default for SynthesisPolicy {
    fn default() -> Self {
        Self { max_session_chars: 8_000, max_insights: 10, include_seed_insights: true }
    }
}

/// Assembles the LLM prompt — pure computation, no I/O.
pub struct SynthesisPromptBuilder {
    pub policy: SynthesisPolicy,
}

impl SynthesisPromptBuilder {
    pub fn new(policy: SynthesisPolicy) -> Self {
        Self { policy }
    }

    /// Returns a two-message sequence `[System, User]` ready to send to the LLM.
    pub fn build(
        &self,
        sessions: &[(String, Vec<Message>)],
        seed_insights: &[TraceInsight],
    ) -> Vec<Message> {
        let mut user_content = String::new();

        // --- session traces (compressed to budget) ---------------------------
        user_content.push_str("## Recent Session Traces\n\n");
        let mut chars_used = 0usize;
        'outer: for (session_id, messages) in sessions {
            user_content.push_str(&format!("### Session {}\n", session_id));
            for msg in messages {
                let remaining = self.policy.max_session_chars.saturating_sub(chars_used);
                if remaining == 0 {
                    user_content.push_str("...[truncated]\n");
                    break 'outer;
                }
                let line = format_message(msg, remaining);
                if !line.is_empty() {
                    chars_used += line.len();
                    user_content.push_str(&line);
                    user_content.push('\n');
                }
            }
            user_content.push('\n');
        }

        // --- seed insights (rule-based observations) -------------------------
        if self.policy.include_seed_insights && !seed_insights.is_empty() {
            user_content
                .push_str("## Rule-Based Observations (synthesize and elevate these)\n\n");
            for insight in seed_insights {
                user_content.push_str(&format!(
                    "- [{}] {}\n",
                    insight.kind.tag(),
                    seed_hint(insight)
                ));
            }
            user_content.push('\n');
        }

        // --- task instruction ------------------------------------------------
        user_content.push_str(&format!(
            "## Task\n\n\
             Analyze the traces above and extract up to {} actionable, durable insights \
             that will help this agent perform better in future sessions.\n\n\
             Respond ONLY with valid JSON matching this exact schema — no prose, no fences:\n\
             {{\"insights\":[{{\"text\":\"...\",\"confidence\":0.0}},...]}}\n\n\
             Rules:\n\
             - text: concise and actionable (max 200 chars)\n\
             - confidence: 0.0–1.0 based on evidence strength\n\
             - Focus on patterns, anti-patterns, and best practices\n\
             - Do not copy seed observations verbatim; synthesize or elevate them",
            self.policy.max_insights
        ));

        vec![Message::system(SYSTEM_PROMPT), Message::user(user_content)]
    }
}

/// Parses the LLM's JSON response into `TraceInsight` objects — pure computation.
pub struct SynthesisResponseParser;

impl SynthesisResponseParser {
    /// `synthetic_session_id` is a stable tag written into each insight's session_id
    /// so curators downstream can distinguish synthesized vs rule-based insights.
    pub fn parse(synthetic_session_id: &str, content: &str) -> Vec<TraceInsight> {
        if let Some(insights) = try_parse_json(synthetic_session_id, content) {
            if !insights.is_empty() {
                return insights;
            }
        }
        // Fallback: treat the entire response as a single synthesized insight.
        vec![TraceInsight {
            kind: InsightKind::Synthesized { text: content.chars().take(300).collect() },
            confidence: 0.5,
            session_id: synthetic_session_id.to_string(),
        }]
    }
}

// --- private helpers ---------------------------------------------------------

const SYSTEM_PROMPT: &str = "\
You are a memory consolidation engine for an AI agent runtime. \
Your role is to read recent agent session traces and extract durable, \
actionable insights that will help the agent perform better in future sessions. \
Think like a senior engineer running a retrospective: identify patterns, \
anti-patterns, and best practices. \
Respond only with structured JSON — no prose, no markdown fences.";

fn format_message(msg: &Message, budget: usize) -> String {
    match msg.role {
        Role::System => String::new(), // system messages add noise, skip them
        Role::User => {
            let body = truncate(msg.content.as_text().unwrap_or(""), budget.min(400));
            format!("[USER] {}", body)
        }
        Role::Assistant => {
            if msg.tool_calls.is_empty() {
                let body = truncate(msg.content.as_text().unwrap_or(""), budget.min(400));
                format!("[ASST] {}", body)
            } else {
                let tools: Vec<_> = msg.tool_calls.iter().map(|tc| tc.name.as_str()).collect();
                format!("[ASST] → tools: {}", tools.join(", "))
            }
        }
        Role::Tool => "[TOOL] [tool results]".to_string(),
    }
}

fn seed_hint(insight: &TraceInsight) -> String {
    match &insight.kind {
        InsightKind::RepeatedToolError { tool_name, error_count, sample_error } => {
            format!("'{}' errored {} times: {}", tool_name, error_count, sample_error)
        }
        InsightKind::SuccessfulToolSequence { tools, context_hint } => {
            format!("Sequence [{}] succeeded for: {}", tools.join("→"), context_hint)
        }
        InsightKind::LongReasoning { summary_hint } => {
            summary_hint.chars().take(100).collect()
        }
        InsightKind::Synthesized { text } => text.chars().take(100).collect(),
    }
}

fn truncate(s: &str, max: usize) -> String {
    let mut result: String = s.chars().take(max).collect();
    if s.len() > max {
        result.push_str("…");
    }
    result
}

fn try_parse_json(session_id: &str, content: &str) -> Option<Vec<TraceInsight>> {
    // Strip markdown fences if the LLM added them despite instructions.
    let cleaned = content
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();

    let v: serde_json::Value = serde_json::from_str(cleaned).ok()?;
    let arr = v.get("insights")?.as_array()?;

    let insights: Vec<TraceInsight> = arr
        .iter()
        .filter_map(|item| {
            let text = item.get("text")?.as_str()?.to_string();
            if text.is_empty() {
                return None;
            }
            let confidence =
                item.get("confidence")?.as_f64().unwrap_or(0.5).clamp(0.0, 1.0);
            Some(TraceInsight {
                kind: InsightKind::Synthesized {
                    text: text.chars().take(300).collect(),
                },
                confidence,
                session_id: session_id.to_string(),
            })
        })
        .collect();

    Some(insights)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::trace_analyzer::TraceInsight;

    fn seed(tool: &str) -> TraceInsight {
        TraceInsight {
            kind: InsightKind::RepeatedToolError {
                tool_name: tool.to_string(),
                error_count: 2,
                sample_error: "permission denied".to_string(),
            },
            confidence: 0.8,
            session_id: "s1".to_string(),
        }
    }

    #[test]
    fn parses_valid_json_response() {
        let json =
            r#"{"insights":[{"text":"Always check permissions before bash","confidence":0.9}]}"#;
        let insights = SynthesisResponseParser::parse("synthetic", json);
        assert_eq!(insights.len(), 1);
        assert!(matches!(insights[0].kind, InsightKind::Synthesized { .. }));
        assert!((insights[0].confidence - 0.9).abs() < 1e-9);
    }

    #[test]
    fn strips_markdown_fences() {
        let fenced =
            "```json\n{\"insights\":[{\"text\":\"use read_file first\",\"confidence\":0.7}]}\n```";
        let insights = SynthesisResponseParser::parse("synthetic", fenced);
        assert_eq!(insights.len(), 1);
        if let InsightKind::Synthesized { text } = &insights[0].kind {
            assert_eq!(text, "use read_file first");
        } else {
            panic!("wrong kind");
        }
    }

    #[test]
    fn falls_back_on_invalid_json() {
        let prose = "You should always check file permissions before running bash commands.";
        let insights = SynthesisResponseParser::parse("synthetic", prose);
        assert_eq!(insights.len(), 1);
        assert!((insights[0].confidence - 0.5).abs() < 1e-9);
    }

    #[test]
    fn clamps_confidence_above_one() {
        let json = r#"{"insights":[{"text":"tip","confidence":1.5}]}"#;
        let insights = SynthesisResponseParser::parse("synthetic", json);
        assert_eq!(insights[0].confidence, 1.0);
    }

    #[test]
    fn empty_insights_array_triggers_fallback() {
        let json = r#"{"insights":[]}"#;
        let insights = SynthesisResponseParser::parse("synthetic", json);
        // empty array → fallback with the raw string
        assert_eq!(insights.len(), 1);
    }

    #[test]
    fn build_prompt_includes_session_content_and_seeds() {
        let builder = SynthesisPromptBuilder::new(SynthesisPolicy::default());
        let sessions =
            vec![("s1".to_string(), vec![Message::user("fix the authentication bug")])];
        let seeds = vec![seed("bash")];
        let msgs = builder.build(&sessions, &seeds);

        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role, Role::System);
        let user_text = msgs[1].content.as_text().unwrap();
        assert!(user_text.contains("fix the authentication bug"));
        assert!(user_text.contains("bash"));
        assert!(user_text.contains("permission denied"));
    }

    #[test]
    fn build_prompt_respects_session_char_budget() {
        let policy = SynthesisPolicy { max_session_chars: 20, ..Default::default() };
        let builder = SynthesisPromptBuilder::new(policy);
        let long_msg = Message::user("x".repeat(1000));
        let sessions = vec![("s1".to_string(), vec![long_msg])];
        let msgs = builder.build(&sessions, &[]);
        let user_text = msgs[1].content.as_text().unwrap();
        // Session content portion should be capped; prompt itself may be longer.
        assert!(user_text.contains("truncated") || user_text.len() < 2000);
    }
}
