//! Evaluation primitives — the agent's "quality gate" compute.
//!
//! Pure computation in kernel, I/O in SDK. This module provides the **stateless** building blocks
//! for the generate → evaluate → retry quality gate:
//!
//! - [`build_eval_messages`] assembles the impartial-evaluator prompt from a goal + criteria + the
//!   agent's output (the SDK then calls the eval LLM with it).
//! - [`parse_verdict`] parses the LLM's JSON response into a structured [`EvalResult`].
//! - [`verdict_output_schema`] is the JSON Schema for that verdict, used as the `output_schema` of
//!   the eval node in the [`crate::orchestration::workflow::gen_eval`] workflow template.
//!
//! **History (0.5.0 fold, OS-axis #6).** This replaces the former `EvalPipeline` state machine +
//! its public SDK class. The quality gate is now expressed on the workflow substrate: the iterative
//! retry-with-feedback loop is driven by the SDK `HarnessLoop` (the kernel `NodeKind::Loop` re-arms
//! a single node, so per-iteration eval cannot be a static DAG), and the declarative
//! "loop-the-worker-then-verify-with-a-structured-verdict" shape is the `gen_eval` template. Both
//! reuse these primitives, so the verdict shape stays consistent across the two paths.
use crate::types::message::{Content, Message, Role};

// ---------------------------------------------------------------------------
// Input types
// ---------------------------------------------------------------------------

/// A single evaluation criterion with optional weight and required flag.
#[derive(Debug, Clone)]
pub struct Criterion {
    pub text: String,
    /// If true, failing this criterion fails the entire evaluation.
    pub required: bool,
    /// Relative weight for scoring (default 1.0).
    pub weight: f32,
}

impl Criterion {
    pub fn required(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            required: true,
            weight: 1.0,
        }
    }

    pub fn optional(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            required: false,
            weight: 1.0,
        }
    }

    pub fn with_weight(mut self, w: f32) -> Self {
        self.weight = w;
        self
    }
}

impl From<String> for Criterion {
    fn from(s: String) -> Self {
        Self::required(s)
    }
}

impl From<&str> for Criterion {
    fn from(s: &str) -> Self {
        Self::required(s)
    }
}

// ---------------------------------------------------------------------------
// Output types
// ---------------------------------------------------------------------------

/// Per-criterion evaluation result.
#[derive(Debug, Clone)]
pub struct CriterionResult {
    pub criterion: String,
    pub passed: bool,
    /// 0.0–1.0 partial credit score.
    pub score: f32,
    pub feedback: String,
}

/// A skill distilled from a successful run — SDK writes this to `skill_dir`.
#[derive(Debug, Clone)]
pub struct SkillCandidate {
    pub name: String,
    pub description: String,
    pub when_to_use: Option<String>,
    /// Markdown body only (no frontmatter) — SDK assembles the full file.
    pub content: String,
}

/// The structured verdict produced by parsing the eval LLM's JSON response.
#[derive(Debug, Clone)]
pub struct EvalResult {
    pub passed: bool,
    /// Weighted aggregate score across all criteria (0.0–1.0).
    pub overall_score: f32,
    /// Human-readable summary injected into the next attempt's goal.
    pub feedback: String,
    /// Per-criterion breakdown.
    pub details: Vec<CriterionResult>,
    pub skill_candidate: Option<SkillCandidate>,
}

// ---------------------------------------------------------------------------
// Prompt builder
// ---------------------------------------------------------------------------

/// Build the impartial-evaluator messages for one attempt: a system instruction describing the
/// scoring contract + a user message carrying the goal, criteria, and the agent's output. The SDK
/// calls the eval LLM with these, then feeds the response to [`parse_verdict`].
pub fn build_eval_messages(
    goal: &str,
    criteria: &[Criterion],
    result: &str,
    attempt: u32,
    extract_skill_on_pass: bool,
) -> Vec<Message> {
    let criteria_text = if criteria.is_empty() {
        "No explicit criteria — use general quality judgement.".to_string()
    } else {
        criteria
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let tag = if c.required {
                    "[required]"
                } else {
                    "[optional]"
                };
                let weight = if (c.weight - 1.0).abs() > 0.01 {
                    format!(" weight={:.1}", c.weight)
                } else {
                    String::new()
                };
                format!("{}. {}{}{}", i + 1, tag, weight, c.text)
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    let details_schema = r#"[{"criterion":"...","passed":bool,"score":0.0-1.0,"feedback":"..."}]"#;

    let skill_instruction = if extract_skill_on_pass {
        "\nIf passed=true and the approach is reusable, add a \"skill\" field:\
\n{\"name\":\"snake_case\",\"description\":\"one sentence\",\"when_to_use\":\"optional hint\",\"content\":\"markdown body (no frontmatter)\"}"
    } else {
        ""
    };

    let system = Message {
        role: Role::System,
        content: Content::Text(format!(
            "You are an impartial evaluator. Assess whether the agent's output meets the goal and criteria.\n\
             [required] criteria must ALL pass for overall passed=true.\n\
             [optional] criteria contribute to overall_score but do not block passing.\n\
             Respond with JSON only:\n\
             {{\"passed\":bool,\"overall_score\":0.0-1.0,\"feedback\":\"concise summary\",\
             \"details\":{details_schema}{skill_instruction}}}"
        )),
        tool_calls: vec![],
        token_count: None,
    };

    let user = Message {
        role: Role::User,
        content: Content::Text(format!(
            "## Goal\n{goal}\n\n## Criteria\n{criteria_text}\n\n## Agent Output (attempt {attempt})\n{result}"
        )),
        tool_calls: vec![],
        token_count: None,
    };

    vec![system, user]
}

// ---------------------------------------------------------------------------
// Verdict output schema (for the gen_eval workflow template's eval node)
// ---------------------------------------------------------------------------

/// JSON Schema for the verdict an eval node must produce. Used as the `output_schema` of the eval
/// node in the [`crate::orchestration::workflow::gen_eval`] template so the SDK can instruct +
/// validate the verdict. Matches what [`parse_verdict`] reads.
pub fn verdict_output_schema(extract_skill_on_pass: bool) -> serde_json::Value {
    let mut properties = serde_json::json!({
        "passed": { "type": "boolean", "description": "true iff all [required] criteria pass" },
        "overall_score": { "type": "number", "minimum": 0.0, "maximum": 1.0 },
        "feedback": { "type": "string", "description": "concise summary; on fail, what to fix next attempt" },
        "details": {
            "type": "array",
            "items": {
                "type": "object",
                "required": ["criterion", "passed", "score", "feedback"],
                "properties": {
                    "criterion": { "type": "string" },
                    "passed": { "type": "boolean" },
                    "score": { "type": "number", "minimum": 0.0, "maximum": 1.0 },
                    "feedback": { "type": "string" }
                }
            }
        }
    });
    if extract_skill_on_pass {
        properties["skill"] = serde_json::json!({
            "type": "object",
            "description": "optional reusable skill distilled from a passing run",
            "required": ["name", "description", "content"],
            "properties": {
                "name": { "type": "string", "description": "snake_case" },
                "description": { "type": "string" },
                "when_to_use": { "type": "string" },
                "content": { "type": "string", "description": "markdown body, no frontmatter" }
            }
        });
    }
    serde_json::json!({
        "type": "object",
        "required": ["passed", "overall_score", "feedback"],
        "properties": properties
    })
}

// ---------------------------------------------------------------------------
// Response parser
// ---------------------------------------------------------------------------

/// Parse an eval LLM's JSON response into a structured [`EvalResult`]. Tolerant of markdown fences
/// and missing fields (defaults: `passed=false`, score derived from `passed`).
pub fn parse_verdict(content: &str) -> EvalResult {
    let json_str = extract_json(content);
    let v: serde_json::Value = serde_json::from_str(json_str).unwrap_or(serde_json::Value::Null);

    let passed = v.get("passed").and_then(|x| x.as_bool()).unwrap_or(false);
    let overall_score = v
        .get("overall_score")
        .and_then(|x| x.as_f64())
        .map(|f| f as f32)
        .unwrap_or(if passed { 1.0 } else { 0.0 });
    let feedback = v
        .get("feedback")
        .and_then(|x| x.as_str())
        .unwrap_or("No feedback provided.")
        .to_string();

    let details = v
        .get("details")
        .and_then(|d| d.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| {
                    let criterion = item.get("criterion")?.as_str()?.to_string();
                    let item_passed = item
                        .get("passed")
                        .and_then(|x| x.as_bool())
                        .unwrap_or(false);
                    let score = item
                        .get("score")
                        .and_then(|x| x.as_f64())
                        .map(|f| f as f32)
                        .unwrap_or(if item_passed { 1.0 } else { 0.0 });
                    let item_feedback = item
                        .get("feedback")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string();
                    Some(CriterionResult {
                        criterion,
                        passed: item_passed,
                        score,
                        feedback: item_feedback,
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    let skill_candidate = v.get("skill").and_then(|s| {
        let name = s.get("name")?.as_str()?.to_string();
        let description = s.get("description")?.as_str()?.to_string();
        let content = s.get("content")?.as_str()?.to_string();
        if name.is_empty() {
            return None;
        }
        let when_to_use = s
            .get("when_to_use")
            .and_then(|x| x.as_str())
            .filter(|x| !x.is_empty())
            .map(|x| x.to_string());
        Some(SkillCandidate {
            name,
            description,
            when_to_use,
            content,
        })
    });

    EvalResult {
        passed,
        overall_score,
        feedback,
        details,
        skill_candidate,
    }
}

fn extract_json(s: &str) -> &str {
    // Strip ```json ... ``` fences if present.
    if let Some(start) = s.find('{') {
        if let Some(end) = s.rfind('}') {
            return &s[start..=end];
        }
    }
    s
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_eval_messages_carries_goal_and_criteria() {
        let msgs = build_eval_messages(
            "Write a function",
            &[Criterion::required("Must handle errors")],
            "fn foo() {}",
            1,
            true,
        );
        assert_eq!(msgs.len(), 2);
        assert!(matches!(msgs[0].role, Role::System));
        let Content::Text(user) = &msgs[1].content else {
            panic!("expected text")
        };
        assert!(user.contains("Write a function"));
        assert!(user.contains("[required]Must handle errors"));
        assert!(user.contains("attempt 1"));
        // skill instruction present when extract_skill_on_pass=true
        let Content::Text(system) = &msgs[0].content else {
            panic!("expected text")
        };
        assert!(system.contains("\"skill\""));
    }

    #[test]
    fn build_eval_messages_omits_skill_instruction_when_disabled() {
        let msgs = build_eval_messages("g", &[], "r", 1, false);
        let Content::Text(system) = &msgs[0].content else {
            panic!("expected text")
        };
        assert!(!system.contains("\"name\":\"snake_case\""));
    }

    #[test]
    fn parse_verdict_failed_no_skill() {
        let result = parse_verdict(
            r#"{"passed":false,"overall_score":0.2,"feedback":"Missing error handling","details":[{"criterion":"Must handle errors","passed":false,"score":0.2,"feedback":"No error handling found"}]}"#,
        );
        assert!(!result.passed);
        assert_eq!(result.feedback, "Missing error handling");
        assert_eq!(result.details.len(), 1);
        assert!(!result.details[0].passed);
        assert!(result.skill_candidate.is_none());
    }

    #[test]
    fn parse_verdict_passed_with_skill_and_details() {
        let json = r#"{"passed":true,"overall_score":0.95,"feedback":"All criteria met","details":[{"criterion":"Must handle errors","passed":true,"score":1.0,"feedback":"Good error handling"}],"skill":{"name":"robust_api_call","description":"How to call APIs with retries","content":"Robust API Call - Always retry on 5xx."}}"#;
        let result = parse_verdict(json);
        assert!(result.passed);
        assert!(result.overall_score > 0.9);
        assert_eq!(result.details.len(), 1);
        assert!(result.details[0].passed);
        let skill = result.skill_candidate.unwrap();
        assert_eq!(skill.name, "robust_api_call");
        assert!(skill.content.contains("retry"));
    }

    #[test]
    fn parse_verdict_strips_markdown_fences() {
        let result = parse_verdict("```json\n{\"passed\":true,\"feedback\":\"good\"}\n```");
        assert!(result.passed);
    }

    #[test]
    fn criterion_from_string_is_required() {
        let c = Criterion::from("some check");
        assert!(c.required);
        assert!((c.weight - 1.0).abs() < 0.001);
    }

    #[test]
    fn optional_criterion_with_weight() {
        let c = Criterion::optional("bonus check").with_weight(0.5);
        assert!(!c.required);
        assert!((c.weight - 0.5).abs() < 0.001);
    }

    #[test]
    fn verdict_output_schema_shape() {
        let schema = verdict_output_schema(true);
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["passed"].is_object());
        assert!(schema["properties"]["overall_score"].is_object());
        assert!(schema["properties"]["details"].is_object());
        assert!(schema["properties"]["skill"].is_object());
        // skill property dropped when extraction is disabled
        let no_skill = verdict_output_schema(false);
        assert!(no_skill["properties"]["skill"].is_null());
    }
}
