use deepstrike_core::harness::{
    build_eval_messages, parse_verdict, verdict_output_schema, Criterion,
};

// ─── Eval prompt builder ────────────────────────────────────────────────────

#[test]
fn build_eval_messages_contain_goal_and_criteria() {
    let messages = build_eval_messages(
        "Write tests",
        &[Criterion::required("Cover edge cases")],
        "test code",
        1,
        true,
    );
    let all_text: String = messages
        .iter()
        .filter_map(|m| m.content.as_text())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(all_text.contains("Write tests"));
    assert!(all_text.contains("Cover edge cases"));
    assert!(all_text.contains("test code"));
    // attempt number surfaces in the output header
    assert!(all_text.contains("attempt 1"));
}

#[test]
fn build_eval_messages_skill_instruction_toggles() {
    let with_skill: String = build_eval_messages("g", &[], "r", 1, true)
        .iter()
        .filter_map(|m| m.content.as_text())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(with_skill.contains("\"skill\""));
    let no_skill: String = build_eval_messages("g", &[], "r", 1, false)
        .iter()
        .filter_map(|m| m.content.as_text())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(!no_skill.contains("\"name\":\"snake_case\""));
}

// ─── Verdict parsing ─────────────────────────────────────────────────────────

#[test]
fn parse_verdict_passed() {
    let result = parse_verdict(r#"{"passed": true, "feedback": "All criteria met"}"#);
    assert!(result.passed);
    assert_eq!(result.feedback, "All criteria met");
    assert!(result.skill_candidate.is_none());
}

#[test]
fn parse_verdict_failed() {
    let result = parse_verdict(r#"{"passed": false, "feedback": "Missing error handling"}"#);
    assert!(!result.passed);
    assert_eq!(result.feedback, "Missing error handling");
}

#[test]
fn parse_verdict_with_skill_candidate() {
    let json = r#"{"passed":true,"feedback":"Good","skill":{"name":"robust_api","description":"API with retries","content":"Always retry on 5xx."}}"#;
    let result = parse_verdict(json);
    assert!(result.passed);
    let skill = result.skill_candidate.unwrap();
    assert_eq!(skill.name, "robust_api");
    assert_eq!(skill.description, "API with retries");
    assert!(skill.content.contains("retry"));
}

#[test]
fn parse_verdict_skill_with_when_to_use() {
    let json = r#"{"passed":true,"feedback":"ok","skill":{"name":"retry","description":"retry logic","when_to_use":"When calling external APIs","content":"body"}}"#;
    let result = parse_verdict(json);
    let skill = result.skill_candidate.unwrap();
    assert_eq!(
        skill.when_to_use.as_deref(),
        Some("When calling external APIs")
    );
}

#[test]
fn parse_verdict_strips_markdown_json_fences() {
    let result = parse_verdict("```json\n{\"passed\":true,\"feedback\":\"good\"}\n```");
    assert!(result.passed);
}

#[test]
fn parse_verdict_handles_malformed_json() {
    let result = parse_verdict("not json at all");
    assert!(!result.passed); // defaults to false
}

// ─── Verdict output schema (gen_eval eval-node contract) ──────────────────────

#[test]
fn verdict_output_schema_shape() {
    let schema = verdict_output_schema(true);
    assert_eq!(schema["type"], "object");
    assert!(schema["properties"]["passed"].is_object());
    assert!(schema["properties"]["overall_score"].is_object());
    assert!(schema["properties"]["feedback"].is_object());
    assert!(schema["properties"]["details"].is_object());
    assert!(schema["properties"]["skill"].is_object());
    // skill property is dropped when extraction is disabled
    assert!(verdict_output_schema(false)["properties"]["skill"].is_null());
}

// ─── SDK AttemptLoop contract ───────────────────────────────────────────────

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use deepstrike_sdk::{
    AttemptBody, AttemptBodyContext, AttemptBodyEvent, AttemptBodyStream, AttemptLoop,
    AttemptOutcomeKind, AttemptRequest, HybridJudge, JudgeContext, StopPolicy, Verdict,
    VerdictFnJudge,
};

#[derive(Clone)]
struct ScriptedBody {
    script: Arc<Mutex<VecDeque<(String, String, u32, u64)>>>,
    contexts: Arc<Mutex<Vec<AttemptBodyContext>>>,
}

impl AttemptBody for ScriptedBody {
    fn run<'a>(&'a self, context: AttemptBodyContext) -> AttemptBodyStream<'a> {
        self.contexts.lock().unwrap().push(context);
        let (result, run_status, turns, tokens) = self.script.lock().unwrap().pop_front().unwrap();
        Box::pin(futures::stream::iter(vec![Ok(
            AttemptBodyEvent::BodyDone {
                run_status,
                result,
                turns,
                total_tokens: tokens,
            },
        )]))
    }
}

fn body(
    script: Vec<(&str, &str, u32, u64)>,
) -> (ScriptedBody, Arc<Mutex<Vec<AttemptBodyContext>>>) {
    let contexts = Arc::new(Mutex::new(Vec::new()));
    (
        ScriptedBody {
            script: Arc::new(Mutex::new(
                script
                    .into_iter()
                    .map(|(result, status, turns, tokens)| {
                        (result.to_string(), status.to_string(), turns, tokens)
                    })
                    .collect(),
            )),
            contexts: contexts.clone(),
        },
        contexts,
    )
}

fn verdict(passed: bool, feedback: &str) -> Verdict {
    Verdict {
        passed,
        overall_score: if passed { 1.0 } else { 0.0 },
        feedback: feedback.to_string(),
        details: vec![],
    }
}

#[tokio::test]
async fn attempt_loop_defaults_to_continue_session_and_carries_feedback_as_context() {
    let (body, contexts) = body(vec![
        ("draft", "completed", 1, 10),
        ("final", "completed", 2, 20),
    ]);
    let judge = VerdictFnJudge::new(Arc::new(|context: &JudgeContext| {
        Some(if context.attempt == 2 {
            verdict(true, "ok")
        } else {
            verdict(false, "fix the assertion")
        })
    }));
    let attempt_loop = AttemptLoop::new(body, judge, StopPolicy::new(2)).unwrap();

    let outcome = attempt_loop
        .run(AttemptRequest::new("stable-session", "original goal"))
        .await
        .unwrap();

    let contexts = contexts.lock().unwrap();
    assert_eq!(contexts.len(), 2);
    assert_eq!(contexts[0].session_id, "stable-session");
    assert_eq!(contexts[1].session_id, "stable-session");
    assert_eq!(contexts[0].goal, "original goal");
    assert_eq!(contexts[1].goal, "original goal");
    assert_eq!(
        contexts[1].context_input.as_deref(),
        Some("fix the assertion")
    );
    assert_eq!(outcome.outcome, AttemptOutcomeKind::Passed);
    assert_eq!(outcome.result, "final");
    assert_eq!(outcome.attempts, 2);
    assert_eq!(outcome.turns, 3);
    assert_eq!(outcome.total_tokens, 30);
}

#[tokio::test]
async fn attempt_loop_keeps_run_health_and_failed_judge_independent() {
    let (body, _) = body(vec![("healthy output", "completed", 1, 8)]);
    let judge = VerdictFnJudge::new(Arc::new(|_: &JudgeContext| {
        Some(verdict(false, "criterion failed"))
    }));
    let attempt_loop =
        AttemptLoop::new(body, judge, StopPolicy::new(3).stop_on_failed_verdict(true)).unwrap();

    let outcome = attempt_loop
        .run(AttemptRequest::new("one-shot", "goal"))
        .await
        .unwrap();

    assert_eq!(outcome.outcome, AttemptOutcomeKind::FailedJudge);
    assert_eq!(outcome.run_status, "completed");
    assert_eq!(outcome.verdict.as_ref().map(|v| v.passed), Some(false));
}

#[tokio::test]
async fn attempt_loop_allows_partial_run_health_to_be_judged() {
    let (body, _) = body(vec![("useful partial", "max_turns", 4, 40)]);
    let judge = VerdictFnJudge::new(Arc::new(|_: &JudgeContext| {
        Some(verdict(true, "partial result is sufficient"))
    }));
    let attempt_loop = AttemptLoop::new(body, judge, StopPolicy::new(1)).unwrap();

    let outcome = attempt_loop
        .run(AttemptRequest::new("partial", "goal"))
        .await
        .unwrap();

    assert_eq!(outcome.outcome, AttemptOutcomeKind::Passed);
    assert_eq!(outcome.run_status, "max_turns");
    assert!(outcome.verdict.unwrap().passed);
}

#[tokio::test]
async fn attempt_loop_does_not_judge_run_errors() {
    let (body, _) = body(vec![("partial", "error", 1, 7)]);
    let calls = Arc::new(Mutex::new(0u32));
    let calls_for_judge = calls.clone();
    let judge = VerdictFnJudge::new(Arc::new(move |_: &JudgeContext| {
        *calls_for_judge.lock().unwrap() += 1;
        Some(verdict(true, "unused"))
    }));
    let attempt_loop = AttemptLoop::new(body, judge, StopPolicy::new(3)).unwrap();

    let outcome = attempt_loop
        .run(AttemptRequest::new("error", "goal"))
        .await
        .unwrap();

    assert_eq!(outcome.outcome, AttemptOutcomeKind::RunError);
    assert_eq!(*calls.lock().unwrap(), 0);
    assert!(outcome.verdict.is_none());
}

#[tokio::test]
async fn hybrid_judge_uses_fallback_only_when_primary_defers() {
    let (body, _) = body(vec![("answer", "completed", 1, 3)]);
    let primary = VerdictFnJudge::new(Arc::new(|_: &JudgeContext| None));
    let fallback = VerdictFnJudge::new(Arc::new(|_: &JudgeContext| {
        Some(verdict(true, "fallback accepted"))
    }));
    let attempt_loop = AttemptLoop::new(
        body,
        HybridJudge::new(primary, fallback),
        StopPolicy::new(1),
    )
    .unwrap();

    let outcome = attempt_loop
        .run(AttemptRequest::new("hybrid", "goal"))
        .await
        .unwrap();

    assert_eq!(outcome.outcome, AttemptOutcomeKind::Passed);
    assert_eq!(outcome.verdict.unwrap().feedback, "fallback accepted");
}
