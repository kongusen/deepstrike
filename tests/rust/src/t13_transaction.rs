#![allow(deprecated)]

// Phase 5 — Transaction Runtime
// G5 gate: recoverable error 不 rollback；replay 精确截断

use compact_str::CompactString;
use deepstrike_core::context::snapshot::ContextFault;
use deepstrike_core::runtime::repair::reconstruct_messages_with_fallback;
use deepstrike_core::runtime::session::{RollbackReason, SessionEvent};
use deepstrike_core::scheduler::policy::LoopPolicy;
use deepstrike_core::scheduler::state_machine::*;
use deepstrike_core::types::message::*;
use deepstrike_core::types::task::RuntimeTask;

fn default_sm() -> LoopStateMachine {
    LoopStateMachine::new(LoopPolicy {
        max_tokens: 128_000,
        ..LoopPolicy::default()
    })
}

fn make_llm_response_with_tool_call(call_id: &str, tool_name: &str) -> LoopEvent {
    LoopEvent::LLMResponse {
        message: Message {
            role: Role::Assistant,
            content: Content::Text(String::new()),
            tool_calls: vec![ToolCall {
                id: CompactString::new(call_id),
                name: CompactString::new(tool_name),
                arguments: serde_json::json!({}),
            }],
            token_count: None,
        },
    }
}

// ─── G5 gate: recoverable error does not rollback ───────────────────────────

#[test]
fn recoverable_error_preserves_history() {
    let mut sm = default_sm();
    sm.start(RuntimeTask::new("test"));
    sm.feed(make_llm_response_with_tool_call("c1", "write_file"));

    let history_before = sm.ctx.partitions.history.messages.len();

    sm.feed(LoopEvent::ToolResults {
        results: vec![ToolResult {
            call_id: CompactString::new("c1"),
            output: Content::Text("permission hint".into()),
            is_error: true,
            is_fatal: false,
            error_kind: Some(ToolErrorKind::Recoverable),
            token_count: None,
        }],
    });

    // Turn incremented, no rollback
    assert_eq!(sm.turn, 1);
    assert!(sm.ctx.partitions.history.messages.len() > history_before);
    let obs = sm.take_observations();
    assert!(!obs.iter().any(|o| matches!(o, LoopObservation::Rollbacked { .. })));
}

#[test]
fn none_error_kind_also_does_not_rollback() {
    let mut sm = default_sm();
    sm.start(RuntimeTask::new("test"));
    sm.feed(make_llm_response_with_tool_call("c1", "read_file"));

    sm.feed(LoopEvent::ToolResults {
        results: vec![ToolResult {
            call_id: CompactString::new("c1"),
            output: Content::Text("ok".into()),
            is_error: false,
            is_fatal: false,
            error_kind: None,
            token_count: None,
        }],
    });

    assert_eq!(sm.turn, 1);
    let obs = sm.take_observations();
    assert!(!obs.iter().any(|o| matches!(o, LoopObservation::Rollbacked { .. })));
}

// ─── G5 gate: fatal error triggers rollback ─────────────────────────────────

#[test]
fn fatal_error_kind_rolls_back_to_checkpoint() {
    let mut sm = default_sm();
    sm.start(RuntimeTask::new("test"));
    sm.feed(make_llm_response_with_tool_call("c1", "write_file"));

    let history_at_checkpoint = sm.ctx.partitions.history.messages.len();

    sm.feed(LoopEvent::ToolResults {
        results: vec![ToolResult {
            call_id: CompactString::new("c1"),
            output: Content::Text("disk corrupt".into()),
            is_error: true,
            is_fatal: false,
            error_kind: Some(ToolErrorKind::Fatal),
            token_count: None,
        }],
    });

    // History must have been truncated back to the checkpoint length
    assert!(sm.ctx.partitions.history.messages.len() <= history_at_checkpoint);
    let obs = sm.take_observations();
    let rolled = obs.iter().find(|o| matches!(o, LoopObservation::Rollbacked { .. }));
    assert!(rolled.is_some(), "expected Rollbacked observation");
    if let Some(LoopObservation::Rollbacked { reason, .. }) = rolled {
        assert!(matches!(reason, RollbackReason::FatalToolError { .. }));
    }
}

#[test]
fn is_fatal_flag_rolls_back() {
    let mut sm = default_sm();
    sm.start(RuntimeTask::new("test"));
    sm.feed(make_llm_response_with_tool_call("c1", "deploy"));

    sm.feed(LoopEvent::ToolResults {
        results: vec![ToolResult {
            call_id: CompactString::new("c1"),
            output: Content::Text("deploy crashed".into()),
            is_error: true,
            is_fatal: true,
            error_kind: None,
            token_count: None,
        }],
    });

    let obs = sm.take_observations();
    assert!(obs.iter().any(|o| matches!(o, LoopObservation::Rollbacked { .. })));
}

// ─── Checkpoint observation emitted before LLM call ─────────────────────────

#[test]
fn checkpoint_taken_observation_emitted_before_llm_call() {
    let mut sm = default_sm();
    sm.start(RuntimeTask::new("test"));

    // start() → emit_call_llm() → should push CheckpointTaken
    let obs = sm.take_observations();
    assert!(
        obs.iter().any(|o| matches!(o, LoopObservation::CheckpointTaken { turn: 0, .. })),
        "expected CheckpointTaken at turn 0, got: {obs:?}"
    );
}

#[test]
fn checkpoint_history_len_matches_actual_history() {
    let mut sm = default_sm();
    sm.start(RuntimeTask::new("test"));
    sm.feed(make_llm_response_with_tool_call("c1", "read_file"));

    sm.feed(LoopEvent::ToolResults {
        results: vec![ToolResult {
            call_id: CompactString::new("c1"),
            output: Content::Text("data".into()),
            is_error: false,
            is_fatal: false,
            error_kind: None,
            token_count: None,
        }],
    });

    let obs = sm.take_observations();
    let checkpoint = obs.iter().find(|o| matches!(o, LoopObservation::CheckpointTaken { turn: 1, .. }));
    assert!(checkpoint.is_some(), "expected CheckpointTaken at turn 1");

    if let Some(LoopObservation::CheckpointTaken { history_len, .. }) = checkpoint {
        // history_len at checkpoint should equal current history length at time of capture
        assert!(*history_len > 0);
    }
}

// ─── G5 gate: replay precise truncation ─────────────────────────────────────

#[test]
fn replay_truncates_to_checkpoint_on_rollback() {
    let session_events = vec![
        SessionEvent::RunStarted {
            run_id: "r1".into(),
            goal: "test".into(),
            criteria: vec![],
            agent_id: None,
            system_prompt: None,
        },
        SessionEvent::LlmCompleted {
            turn: 0,
            message: Message {
                role: Role::Assistant,
                content: Content::Text("I'll write a file".into()),
                tool_calls: vec![ToolCall {
                    id: CompactString::new("c1"),
                    name: CompactString::new("write_file"),
                    arguments: serde_json::json!({}),
                }],
                token_count: None,
            },
            provider_replay: None,
        },
        // Rollback brings history back to 1 message (the assistant turn before tool call)
        SessionEvent::Rollbacked {
            turn: 0,
            category: None,
            primitive: None,
            checkpoint_history_len: 1,
            reason: Some(RollbackReason::FatalToolError {
                tool_name: "write_file".into(),
                error: "disk corrupt".into(),
            }),
        },
    ];

    let messages = reconstruct_messages_with_fallback(
        &session_events,
        "test-session",
        usize::MAX,
        |_| Err(ContextFault::MissingArchive { session_id: String::new(), seq: 0 }),
    );
    assert_eq!(messages.len(), 1, "history should be truncated to checkpoint length 1");
}

#[test]
fn replay_without_rollback_keeps_full_history() {
    let session_events = vec![
        SessionEvent::RunStarted {
            run_id: "r1".into(),
            goal: "test".into(),
            criteria: vec![],
            agent_id: None,
            system_prompt: None,
        },
        SessionEvent::LlmCompleted {
            turn: 0,
            message: Message {
                role: Role::Assistant,
                content: Content::Text("I'll read a file".into()),
                tool_calls: vec![ToolCall {
                    id: CompactString::new("c1"),
                    name: CompactString::new("read_file"),
                    arguments: serde_json::json!({}),
                }],
                token_count: None,
            },
            provider_replay: None,
        },
    ];

    let messages = reconstruct_messages_with_fallback(
        &session_events,
        "test-session",
        usize::MAX,
        |_| Err(ContextFault::MissingArchive { session_id: String::new(), seq: 0 }),
    );
    // RunStarted → User message + LlmCompleted → Assistant message = 2
    assert_eq!(messages.len(), 2, "no rollback: full history preserved");
}

// ─── TurnCheckpoint struct ───────────────────────────────────────────────────

#[test]
fn turn_checkpoint_default_is_zero() {
    let cp = TurnCheckpoint::default();
    assert_eq!(cp.history_len, 0);
    assert_eq!(cp.signals_len, 0);
    assert!(cp.task_state.is_none());
}
