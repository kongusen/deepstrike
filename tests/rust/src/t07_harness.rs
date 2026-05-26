use deepstrike_core::harness::eval_pipeline::*;

// ─── EvalPipeline lifecycle ─────────────────────────────────────────────────

#[test]
fn pipeline_starts_idle() {
    let p = EvalPipeline::new(EvalPolicy::default());
    assert!(p.is_idle());
}

#[test]
fn outcome_transitions_to_eval_pending() {
    let mut p = EvalPipeline::new(EvalPolicy::default());
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
fn evaluate_messages_contain_goal_and_criteria() {
    let mut p = EvalPipeline::new(EvalPolicy::default());
    let action = p.feed(EvalEvent::Outcome {
        goal: "Write tests".into(),
        criteria: vec!["Cover edge cases".into()],
        result: "test code".into(),
        attempt: 1,
    });
    match action {
        EvalAction::Evaluate { messages } => {
            let all_text: String = messages
                .iter()
                .filter_map(|m| m.content.as_text())
                .collect::<Vec<_>>()
                .join(" ");
            assert!(all_text.contains("Write tests"));
            assert!(all_text.contains("Cover edge cases"));
            assert!(all_text.contains("test code"));
        }
        _ => panic!("expected Evaluate"),
    }
}

// ─── Eval result parsing ────────────────────────────────────────────────────

#[test]
fn eval_result_passed() {
    let mut p = EvalPipeline::new(EvalPolicy::default());
    p.feed(EvalEvent::Outcome {
        goal: "g".into(),
        criteria: vec![],
        result: "r".into(),
        attempt: 1,
    });
    let action = p.feed(EvalEvent::EvalResult {
        content: r#"{"passed": true, "feedback": "All criteria met"}"#.into(),
    });
    match action {
        EvalAction::Done { result } => {
            assert!(result.passed);
            assert_eq!(result.feedback, "All criteria met");
            assert!(result.skill_candidate.is_none());
        }
        _ => panic!("expected Done"),
    }
}

#[test]
fn eval_result_failed() {
    let mut p = EvalPipeline::new(EvalPolicy::default());
    p.feed(EvalEvent::Outcome {
        goal: "g".into(),
        criteria: vec![],
        result: "r".into(),
        attempt: 1,
    });
    let action = p.feed(EvalEvent::EvalResult {
        content: r#"{"passed": false, "feedback": "Missing error handling"}"#.into(),
    });
    match action {
        EvalAction::Done { result } => {
            assert!(!result.passed);
            assert_eq!(result.feedback, "Missing error handling");
        }
        _ => panic!("expected Done"),
    }
}

#[test]
fn eval_result_with_skill_candidate() {
    let mut p = EvalPipeline::new(EvalPolicy {
        extract_skill_on_pass: true,
    });
    p.feed(EvalEvent::Outcome {
        goal: "g".into(),
        criteria: vec![],
        result: "r".into(),
        attempt: 1,
    });
    let json = r#"{"passed":true,"feedback":"Good","skill":{"name":"robust_api","description":"API with retries","content":"Always retry on 5xx."}}"#;
    let action = p.feed(EvalEvent::EvalResult {
        content: json.into(),
    });
    match action {
        EvalAction::Done { result } => {
            assert!(result.passed);
            let skill = result.skill_candidate.unwrap();
            assert_eq!(skill.name, "robust_api");
            assert_eq!(skill.description, "API with retries");
            assert!(skill.content.contains("retry"));
        }
        _ => panic!("expected Done"),
    }
}

#[test]
fn eval_result_skill_with_when_to_use() {
    let mut p = EvalPipeline::new(EvalPolicy {
        extract_skill_on_pass: true,
    });
    p.feed(EvalEvent::Outcome {
        goal: "g".into(),
        criteria: vec![],
        result: "r".into(),
        attempt: 1,
    });
    let json = r#"{"passed":true,"feedback":"ok","skill":{"name":"retry","description":"retry logic","when_to_use":"When calling external APIs","content":"body"}}"#;
    let action = p.feed(EvalEvent::EvalResult {
        content: json.into(),
    });
    match action {
        EvalAction::Done { result } => {
            let skill = result.skill_candidate.unwrap();
            assert_eq!(
                skill.when_to_use.as_deref(),
                Some("When calling external APIs")
            );
        }
        _ => panic!("expected Done"),
    }
}

// ─── Markdown fence stripping ───────────────────────────────────────────────

#[test]
fn strips_markdown_json_fences() {
    let mut p = EvalPipeline::new(EvalPolicy::default());
    p.feed(EvalEvent::Outcome {
        goal: "g".into(),
        criteria: vec![],
        result: "r".into(),
        attempt: 1,
    });
    let action = p.feed(EvalEvent::EvalResult {
        content: "```json\n{\"passed\":true,\"feedback\":\"good\"}\n```".into(),
    });
    match action {
        EvalAction::Done { result } => assert!(result.passed),
        _ => panic!("expected Done"),
    }
}

#[test]
fn handles_malformed_json() {
    let mut p = EvalPipeline::new(EvalPolicy::default());
    p.feed(EvalEvent::Outcome {
        goal: "g".into(),
        criteria: vec![],
        result: "r".into(),
        attempt: 1,
    });
    let action = p.feed(EvalEvent::EvalResult {
        content: "not json at all".into(),
    });
    match action {
        EvalAction::Done { result } => {
            assert!(!result.passed); // defaults to false
        }
        _ => panic!("expected Done"),
    }
}

// ─── Reset ──────────────────────────────────────────────────────────────────

#[test]
fn reset_returns_to_idle() {
    let mut p = EvalPipeline::new(EvalPolicy::default());
    p.feed(EvalEvent::Outcome {
        goal: "g".into(),
        criteria: vec![],
        result: "r".into(),
        attempt: 1,
    });
    p.feed(EvalEvent::EvalResult {
        content: r#"{"passed":true,"feedback":"ok"}"#.into(),
    });
    assert!(!p.is_idle());
    p.reset();
    assert!(p.is_idle());
}

#[test]
fn reset_allows_reuse() {
    let mut p = EvalPipeline::new(EvalPolicy::default());
    p.feed(EvalEvent::Outcome {
        goal: "g".into(),
        criteria: vec![],
        result: "r".into(),
        attempt: 1,
    });
    p.feed(EvalEvent::EvalResult {
        content: r#"{"passed":false,"feedback":"fail"}"#.into(),
    });
    p.reset();

    let action = p.feed(EvalEvent::Outcome {
        goal: "g2".into(),
        criteria: vec!["c1".into()],
        result: "r2".into(),
        attempt: 2,
    });
    assert!(matches!(action, EvalAction::Evaluate { .. }));
}

// ─── EvalPolicy ─────────────────────────────────────────────────────────────

#[test]
fn eval_policy_default() {
    let policy = EvalPolicy::default();
    assert!(policy.extract_skill_on_pass);
}

#[test]
fn eval_policy_no_skill_extraction() {
    let mut p = EvalPipeline::new(EvalPolicy {
        extract_skill_on_pass: false,
    });
    p.feed(EvalEvent::Outcome {
        goal: "g".into(),
        criteria: vec![],
        result: "r".into(),
        attempt: 1,
    });
    let action = p.feed(EvalEvent::EvalResult {
        content: r#"{"passed":true,"feedback":"ok"}"#.into(),
    });
    match action {
        EvalAction::Done { result } => {
            assert!(result.passed);
        }
        _ => panic!("expected Done"),
    }
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
