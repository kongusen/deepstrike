/// Evaluation pipeline — the agent's "quality gate" cycle.
///
/// Mirrors `IdlePipeline` in structure: pure computation in kernel, I/O in SDK.
///
/// ```text
/// Phase 1 — Prompt assembly (synchronous, in-kernel)
/// ┌──────────────────────────────────────────────────────┐
/// │ EvalEvent::Outcome { goal, criteria, result }        │
/// │   → build evaluation prompt                          │
/// │   → EvalAction::Evaluate { messages }                │ ← SDK calls LLM
/// └──────────────────────────────────────────────────────┘
///
/// Phase 2 — Parse LLM verdict (after SDK returns)
/// ┌──────────────────────────────────────────────────────┐
/// │ EvalEvent::EvalResult { content }                    │
/// │   → parse JSON → EvalResult { passed, feedback,     │
/// │                               skill_candidate }      │
/// │   → EvalAction::Done { result }                      │ ← SDK acts on result
/// └──────────────────────────────────────────────────────┘
/// ```
use crate::types::message::{Content, Message, Role};

// ---------------------------------------------------------------------------
// Output types
// ---------------------------------------------------------------------------

/// A skill distilled from a successful run — SDK writes this to `skill_dir`.
#[derive(Debug, Clone)]
pub struct SkillCandidate {
    /// Filename stem (no extension). E.g. `"robust_api_call"`.
    pub name: String,
    pub description: String,
    pub when_to_use: Option<String>,
    /// Markdown body only (no frontmatter) — SDK assembles the full file.
    pub content: String,
}

#[derive(Debug, Clone)]
pub struct EvalResult {
    pub passed: bool,
    /// Human-readable explanation injected into the next attempt's goal.
    pub feedback: String,
    /// Present when the run succeeded and the LLM identified a reusable pattern.
    pub skill_candidate: Option<SkillCandidate>,
}

// ---------------------------------------------------------------------------
// State machine types
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum EvalPhase {
    Idle,
    EvalPending { goal: String, attempt: u32 },
    Done,
}

pub enum EvalEvent {
    /// SDK provides the goal, criteria, and the agent's output text.
    Outcome {
        goal: String,
        criteria: Vec<String>,
        result: String,
        attempt: u32,
    },
    /// SDK feeds back the LLM evaluator's text response.
    EvalResult { content: String },
}

pub enum EvalAction {
    /// Call the LLM with `messages`, then feed `EvalEvent::EvalResult`.
    Evaluate { messages: Vec<Message> },
    /// Evaluation complete — SDK reads `result` and decides next step.
    Done { result: EvalResult },
}

// ---------------------------------------------------------------------------
// Policy
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct EvalPolicy {
    /// Whether to ask the LLM to propose a skill when the run passes. Default: true.
    pub extract_skill_on_pass: bool,
}

impl Default for EvalPolicy {
    fn default() -> Self {
        Self { extract_skill_on_pass: true }
    }
}

// ---------------------------------------------------------------------------
// Pipeline
// ---------------------------------------------------------------------------

pub struct EvalPipeline {
    pub phase: EvalPhase,
    policy: EvalPolicy,
}

impl EvalPipeline {
    pub fn new(policy: EvalPolicy) -> Self {
        Self { phase: EvalPhase::Idle, policy }
    }

    pub fn is_idle(&self) -> bool {
        matches!(self.phase, EvalPhase::Idle)
    }

    pub fn feed(&mut self, event: EvalEvent) -> EvalAction {
        match event {
            EvalEvent::Outcome { goal, criteria, result, attempt } => {
                let messages = build_eval_prompt(&goal, &criteria, &result, attempt, &self.policy);
                self.phase = EvalPhase::EvalPending { goal, attempt };
                EvalAction::Evaluate { messages }
            }

            EvalEvent::EvalResult { content } => {
                self.phase = EvalPhase::Done;
                EvalAction::Done { result: parse_eval_response(&content) }
            }
        }
    }

    pub fn reset(&mut self) {
        self.phase = EvalPhase::Idle;
    }
}

// ---------------------------------------------------------------------------
// Prompt builder
// ---------------------------------------------------------------------------

fn build_eval_prompt(
    goal: &str,
    criteria: &[String],
    result: &str,
    attempt: u32,
    policy: &EvalPolicy,
) -> Vec<Message> {
    let criteria_text = if criteria.is_empty() {
        "No explicit criteria — use general quality judgement.".to_string()
    } else {
        criteria.iter().enumerate().map(|(i, c)| format!("{}. {}", i + 1, c)).collect::<Vec<_>>().join("\n")
    };

    let skill_instruction = if policy.extract_skill_on_pass {
        "\nIf passed=true and the approach is reusable, add a \"skill\" field:\
\n{\"name\":\"snake_case\",\"description\":\"one sentence\",\"when_to_use\":\"optional hint\",\"content\":\"markdown body (no frontmatter)\"}"
    } else {
        ""
    };

    let system = Message {
        role: Role::System,
        content: Content::Text(format!(
            "You are an impartial evaluator. Assess whether the agent's output meets the goal and criteria.\n\
             Respond with JSON only:\n\
             {{\"passed\": bool, \"feedback\": \"concise explanation\"{skill_instruction}}}"
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
// Response parser
// ---------------------------------------------------------------------------

fn parse_eval_response(content: &str) -> EvalResult {
    // Extract JSON from possible markdown fences.
    let json_str = extract_json(content);

    let v: serde_json::Value = serde_json::from_str(json_str).unwrap_or(serde_json::Value::Null);

    let passed = v.get("passed").and_then(|x| x.as_bool()).unwrap_or(false);
    let feedback = v.get("feedback").and_then(|x| x.as_str()).unwrap_or("No feedback provided.").to_string();

    let skill_candidate = v.get("skill").and_then(|s| {
        let name = s.get("name")?.as_str()?.to_string();
        let description = s.get("description")?.as_str()?.to_string();
        let content = s.get("content")?.as_str()?.to_string();
        if name.is_empty() { return None; }
        let when_to_use = s.get("when_to_use").and_then(|x| x.as_str()).filter(|x| !x.is_empty()).map(|x| x.to_string());
        Some(SkillCandidate { name, description, when_to_use, content })
    });

    EvalResult { passed, feedback, skill_candidate }
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

    fn pipeline() -> EvalPipeline {
        EvalPipeline::new(EvalPolicy::default())
    }

    #[test]
    fn starts_idle() {
        assert!(pipeline().is_idle());
    }

    #[test]
    fn outcome_emits_evaluate() {
        let mut p = pipeline();
        let action = p.feed(EvalEvent::Outcome {
            goal: "Write a function".into(),
            criteria: vec!["Must handle errors".into()],
            result: "fn foo() {}".into(),
            attempt: 1,
        });
        assert!(matches!(action, EvalAction::Evaluate { .. }));
        assert!(matches!(p.phase, EvalPhase::EvalPending { .. }));
    }

    #[test]
    fn eval_result_failed_no_skill() {
        let mut p = pipeline();
        p.feed(EvalEvent::Outcome {
            goal: "g".into(), criteria: vec![], result: "r".into(), attempt: 1,
        });
        let action = p.feed(EvalEvent::EvalResult {
            content: r#"{"passed": false, "feedback": "Missing error handling"}"#.into(),
        });
        match action {
            EvalAction::Done { result } => {
                assert!(!result.passed);
                assert_eq!(result.feedback, "Missing error handling");
                assert!(result.skill_candidate.is_none());
            }
            _ => panic!("expected Done"),
        }
    }

    #[test]
    fn eval_result_passed_with_skill() {
        let mut p = pipeline();
        p.feed(EvalEvent::Outcome {
            goal: "g".into(), criteria: vec![], result: "r".into(), attempt: 1,
        });
        let json = r#"{"passed":true,"feedback":"All criteria met","skill":{"name":"robust_api_call","description":"How to call APIs with retries","content":"Robust API Call - Always retry on 5xx."}}"#;
        let action = p.feed(EvalEvent::EvalResult { content: json.into() });
        match action {
            EvalAction::Done { result } => {
                assert!(result.passed);
                let skill = result.skill_candidate.unwrap();
                assert_eq!(skill.name, "robust_api_call");
                assert!(skill.content.contains("retry"));
            }
            _ => panic!("expected Done"),
        }
    }

    #[test]
    fn reset_allows_reuse() {
        let mut p = pipeline();
        p.feed(EvalEvent::Outcome {
            goal: "g".into(), criteria: vec![], result: "r".into(), attempt: 1,
        });
        p.feed(EvalEvent::EvalResult { content: r#"{"passed":true,"feedback":"ok"}"#.into() });
        p.reset();
        assert!(p.is_idle());
    }

    #[test]
    fn strips_markdown_fences() {
        let mut p = pipeline();
        p.feed(EvalEvent::Outcome {
            goal: "g".into(), criteria: vec![], result: "r".into(), attempt: 1,
        });
        let action = p.feed(EvalEvent::EvalResult {
            content: "```json\n{\"passed\":true,\"feedback\":\"good\"}\n```".into(),
        });
        match action {
            EvalAction::Done { result } => assert!(result.passed),
            _ => panic!(),
        }
    }
}
