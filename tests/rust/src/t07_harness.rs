use deepstrike_core::harness::{build_eval_messages, parse_verdict, verdict_output_schema, Criterion};

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

// ─── SDK-level Harness types ────────────────────────────────────────────────

#[test]
fn harness_request_builder() {
    let req = deepstrike_sdk::HarnessRequest::new("Write a poem");
    assert_eq!(req.goal, "Write a poem");
    assert!(req.criteria.is_empty());
    assert!(req.extensions.is_none());
}

#[test]
fn harness_outcome_fields() {
    let outcome = deepstrike_sdk::HarnessOutcome {
        result: "A poem".into(),
        passed: true,
        iterations: 1,
        total_tokens: 100,
        status: "completed".into(),
        overall_score: 1.0,
        feedback: Some("Great work!".into()),
        details: vec![],
    };
    assert!(outcome.passed);
    assert_eq!(outcome.iterations, 1);
    assert_eq!(outcome.feedback.as_deref(), Some("Great work!"));
}
