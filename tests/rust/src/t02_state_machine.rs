#![allow(deprecated)]

use compact_str::CompactString;
use deepstrike_core::context::manager::{KNOWLEDGE_TOOL_NAME, MEMORY_TOOL_NAME};
use deepstrike_core::context::skill_catalog::SKILL_TOOL_NAME;
use deepstrike_core::scheduler::policy::LoopPolicy;
use deepstrike_core::scheduler::state_machine::*;
use deepstrike_core::types::message::*;
use deepstrike_core::types::result::TerminationReason;
use deepstrike_core::types::skill::SkillMetadata;
use deepstrike_core::types::task::RuntimeTask;

fn default_sm() -> LoopStateMachine {
    LoopStateMachine::new(LoopPolicy {
        max_tokens: 128_000,
        ..LoopPolicy::default()
    })
}

// ─── Basic lifecycle ────────────────────────────────────────────────────────

#[test]
fn starts_in_idle_phase() {
    let sm = default_sm();
    assert!(matches!(sm.phase, LoopPhase::Idle));
    assert_eq!(sm.turn, 0);
}

#[test]
fn start_transitions_to_reason_and_emits_call_llm() {
    let mut sm = default_sm();
    let action = sm.start(RuntimeTask::new("Say hello"));
    assert!(matches!(action, LoopAction::CallLLM { .. }));
    assert!(matches!(sm.phase, LoopPhase::Reason));
}

#[test]
fn text_only_response_terminates_with_completed() {
    let mut sm = default_sm();
    sm.start(RuntimeTask::new("Say hello"));
    let action = sm.feed(LoopEvent::LLMResponse {
        message: Message::assistant("Hello!"),
    });
    match action {
        LoopAction::Done { result } => {
            assert_eq!(result.termination, TerminationReason::Completed);
            assert!(result.final_message.is_some());
        }
        _ => panic!("expected Done"),
    }
    assert!(sm.is_terminal());
}

#[test]
fn tool_calls_emit_execute_tools() {
    let mut sm = default_sm();
    sm.start(RuntimeTask::new("Calculate"));

    let msg = Message {
        role: Role::Assistant,
        content: Content::Text(String::new()),
        tool_calls: vec![ToolCall {
            id: CompactString::new("c1"),
            name: CompactString::new("add"),
            arguments: serde_json::json!({"x": 1, "y": 2}),
        }],
        token_count: None,
    };

    let action = sm.feed(LoopEvent::LLMResponse { message: msg });
    match action {
        LoopAction::ExecuteTools { calls } => {
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0].name.as_str(), "add");
        }
        _ => panic!("expected ExecuteTools"),
    }
    assert!(matches!(sm.phase, LoopPhase::Act { .. }));
}

#[test]
fn tool_results_advance_turn_and_emit_call_llm() {
    let mut sm = default_sm();
    sm.start(RuntimeTask::new("test"));

    let msg = Message {
        role: Role::Assistant,
        content: Content::Text(String::new()),
        tool_calls: vec![ToolCall {
            id: CompactString::new("c1"),
            name: CompactString::new("add"),
            arguments: serde_json::json!({}),
        }],
        token_count: None,
    };
    sm.feed(LoopEvent::LLMResponse { message: msg });

    let results = vec![ToolResult {
        call_id: CompactString::new("c1"),
        output: Content::Text("3".into()),
        is_error: false,
        is_fatal: false,
        token_count: None,
    }];
    let action = sm.feed(LoopEvent::ToolResults { results });
    assert!(matches!(action, LoopAction::CallLLM { .. }));
    assert_eq!(sm.turn, 1);
}

// ─── Termination policies ───────────────────────────────────────────────────

#[test]
fn max_turns_triggers_pending_termination_then_done() {
    let mut sm = LoopStateMachine::new(LoopPolicy {
        max_tokens: 128_000,
        max_turns: 1,
        ..LoopPolicy::default()
    });
    sm.start(RuntimeTask::new("test"));

    let action = sm.feed(LoopEvent::ToolResults { results: vec![] });
    match &action {
        LoopAction::CallLLM { tools, .. } => {
            assert!(tools.is_empty(), "pending termination strips tools");
        }
        _ => panic!("expected CallLLM"),
    }

    let action = sm.feed(LoopEvent::LLMResponse {
        message: Message::assistant("summary"),
    });
    match action {
        LoopAction::Done { result } => {
            assert_eq!(result.termination, TerminationReason::MaxTurns);
            assert!(result.final_message.is_some());
        }
        _ => panic!("expected Done"),
    }
}

#[test]
fn timeout_terminates_immediately() {
    let mut sm = default_sm();
    sm.start(RuntimeTask::new("test"));
    match sm.feed(LoopEvent::Timeout) {
        LoopAction::Done { result } => {
            assert_eq!(result.termination, TerminationReason::Timeout);
        }
        _ => panic!("expected Done"),
    }
}

// ─── Criteria injection ─────────────────────────────────────────────────────

#[test]
fn criteria_injected_into_user_message() {
    let mut sm = default_sm();
    let task = RuntimeTask::new("Write code")
        .with_criteria(vec!["Must handle errors".into(), "Must be fast".into()]);
    let action = sm.start(task);

    match action {
        LoopAction::CallLLM { context, .. } => {
            let user_msgs: Vec<_> = context
                .turns
                .iter()
                .filter(|m| m.role == Role::User)
                .collect();
            assert!(!user_msgs.is_empty());
            let text = user_msgs.last().unwrap().content.as_text().unwrap();
            assert!(text.contains("Write code"));
            assert!(text.contains("Criteria:"));
            assert!(text.contains("1. Must handle errors"));
            assert!(text.contains("2. Must be fast"));
        }
        _ => panic!("expected CallLLM"),
    }
}

#[test]
fn no_criteria_means_plain_goal() {
    let mut sm = default_sm();
    let action = sm.start(RuntimeTask::new("Say hello"));
    match action {
        LoopAction::CallLLM { context, .. } => {
            let user_text = context
                .turns
                .iter()
                .filter(|m| m.role == Role::User)
                .last()
                .unwrap()
                .content
                .as_text()
                .unwrap();
            assert_eq!(user_text, "Say hello");
            assert!(!user_text.contains("Criteria:"));
        }
        _ => panic!("expected CallLLM"),
    }
}

// ─── Signal handling ────────────────────────────────────────────────────────

#[test]
fn critical_signal_injects_interrupt_and_re_reasons() {
    use deepstrike_core::types::signal::{SignalSource, SignalType, Urgency};

    let mut sm = default_sm();
    sm.start(RuntimeTask::new("test"));

    let sig = deepstrike_core::RuntimeSignal::new(
        SignalSource::Gateway,
        SignalType::Alert,
        Urgency::Critical,
        "fire",
    );
    let action = sm.feed(LoopEvent::Signal { signal: sig });
    assert!(matches!(action, LoopAction::CallLLM { .. }));

    let has_interrupt = sm.ctx.partitions.working.messages.iter().any(|m| {
        m.content
            .as_text()
            .map(|t| t.contains("[INTERRUPT]"))
            .unwrap_or(false)
    });
    assert!(has_interrupt);
}

#[test]
fn high_urgency_signal_injects_note() {
    use deepstrike_core::types::signal::{SignalSource, SignalType, Urgency};

    let mut sm = default_sm();
    sm.start(RuntimeTask::new("test"));

    let sig = deepstrike_core::RuntimeSignal::new(
        SignalSource::Custom,
        SignalType::Event,
        Urgency::High,
        "alert",
    );
    let action = sm.feed(LoopEvent::Signal { signal: sig });
    assert!(matches!(action, LoopAction::CallLLM { .. }));

    let has_signal = sm.ctx.partitions.working.messages.iter().any(|m| {
        m.content
            .as_text()
            .map(|t| t.contains("[SIGNAL]"))
            .unwrap_or(false)
    });
    assert!(has_signal);
}

// ─── Meta-tool injection ────────────────────────────────────────────────────

#[test]
fn skill_tool_injected_when_skills_registered() {
    let mut sm = default_sm();
    sm.ctx
        .set_available_skills(vec![SkillMetadata::new("debug", "Debug helper")]);
    let action = sm.start(RuntimeTask::new("Fix the bug"));
    match action {
        LoopAction::CallLLM { tools, .. } => {
            assert!(tools.iter().any(|t| t.name.as_str() == SKILL_TOOL_NAME));
        }
        _ => panic!("expected CallLLM"),
    }
}

#[test]
fn skill_tool_absent_when_no_skills() {
    let mut sm = default_sm();
    let action = sm.start(RuntimeTask::new("Hello"));
    match action {
        LoopAction::CallLLM { tools, .. } => {
            assert!(!tools.iter().any(|t| t.name.as_str() == SKILL_TOOL_NAME));
        }
        _ => panic!("expected CallLLM"),
    }
}

#[test]
fn memory_tool_injected_when_enabled() {
    let mut sm = default_sm();
    sm.ctx.set_memory_enabled(true);
    let action = sm.start(RuntimeTask::new("Recall"));
    match action {
        LoopAction::CallLLM { tools, .. } => {
            assert!(tools.iter().any(|t| t.name.as_str() == MEMORY_TOOL_NAME));
        }
        _ => panic!("expected CallLLM"),
    }
}

#[test]
fn memory_tool_absent_when_disabled() {
    let mut sm = default_sm();
    let action = sm.start(RuntimeTask::new("Hello"));
    match action {
        LoopAction::CallLLM { tools, .. } => {
            assert!(!tools.iter().any(|t| t.name.as_str() == MEMORY_TOOL_NAME));
        }
        _ => panic!("expected CallLLM"),
    }
}

#[test]
fn knowledge_tool_injected_when_enabled() {
    let mut sm = default_sm();
    sm.ctx.set_knowledge_enabled(true);
    let action = sm.start(RuntimeTask::new("Lookup"));
    match action {
        LoopAction::CallLLM { tools, .. } => {
            assert!(tools.iter().any(|t| t.name.as_str() == KNOWLEDGE_TOOL_NAME));
        }
        _ => panic!("expected CallLLM"),
    }
}

// ─── Observations ───────────────────────────────────────────────────────────

#[test]
fn take_observations_drains() {
    let mut sm = default_sm();
    sm.start(RuntimeTask::new("test"));
    let obs1 = sm.take_observations();
    let obs2 = sm.take_observations();
    assert!(obs1.is_empty() || obs2.is_empty());
}

#[test]
fn compression_emits_compressed_observation() {
    let mut sm = LoopStateMachine::new(LoopPolicy {
        max_tokens: 100,
        max_turns: 100,
        ..LoopPolicy::default()
    });
    sm.start(RuntimeTask::new("test"));
    for i in 0..10 {
        sm.ctx
            .push_history(Message::user(format!("filler message {i}")), 50);
    }
    sm.feed(LoopEvent::ToolResults { results: vec![] });
    let obs = sm.take_observations();
    assert!(
        obs.iter()
            .any(|o| matches!(o, LoopObservation::Compressed { .. }))
    );
}

// ─── Multi-turn loop ────────────────────────────────────────────────────────

#[test]
fn full_tool_cycle_then_text_completes() {
    let mut sm = default_sm();
    sm.start(RuntimeTask::new("Add 1+2"));

    // LLM calls a tool
    let msg = Message {
        role: Role::Assistant,
        content: Content::Text(String::new()),
        tool_calls: vec![ToolCall {
            id: CompactString::new("c1"),
            name: CompactString::new("add"),
            arguments: serde_json::json!({"x": 1, "y": 2}),
        }],
        token_count: Some(10),
    };
    let action = sm.feed(LoopEvent::LLMResponse { message: msg });
    assert!(matches!(action, LoopAction::ExecuteTools { .. }));

    // Tool results
    let results = vec![ToolResult {
        call_id: CompactString::new("c1"),
        output: Content::Text("3".into()),
        is_error: false,
        is_fatal: false,
        token_count: Some(5),
    }];
    let action = sm.feed(LoopEvent::ToolResults { results });
    assert!(matches!(action, LoopAction::CallLLM { .. }));
    assert_eq!(sm.turn, 1);

    // LLM responds with text → done
    let action = sm.feed(LoopEvent::LLMResponse {
        message: Message::assistant("The answer is 3"),
    });
    match action {
        LoopAction::Done { result } => {
            assert_eq!(result.termination, TerminationReason::Completed);
            assert_eq!(result.turns_used, 1);
        }
        _ => panic!("expected Done"),
    }
}

// ─── User tools merged with meta-tools ──────────────────────────────────────

#[test]
fn user_tools_appear_in_call_llm() {
    let mut sm = default_sm();
    sm.tools = vec![ToolSchema {
        name: CompactString::new("read_file"),
        description: "Read a file.".into(),
        parameters: serde_json::json!({"type": "object"}),
    }];
    let action = sm.start(RuntimeTask::new("Read file"));
    match action {
        LoopAction::CallLLM { tools, .. } => {
            assert!(tools.iter().any(|t| t.name.as_str() == "read_file"));
        }
        _ => panic!("expected CallLLM"),
    }
}

#[test]
fn user_tools_plus_skill_tool() {
    let mut sm = default_sm();
    sm.tools = vec![ToolSchema {
        name: CompactString::new("search"),
        description: "Search.".into(),
        parameters: serde_json::json!({}),
    }];
    sm.ctx
        .set_available_skills(vec![SkillMetadata::new("debug", "D")]);

    let action = sm.start(RuntimeTask::new("Debug"));
    match action {
        LoopAction::CallLLM { tools, .. } => {
            assert!(tools.iter().any(|t| t.name.as_str() == "search"));
            assert!(tools.iter().any(|t| t.name.as_str() == SKILL_TOOL_NAME));
        }
        _ => panic!("expected CallLLM"),
    }
}
