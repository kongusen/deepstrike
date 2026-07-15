//! Read-only OS audit snapshot rebuilt from append-only session events (Phase 6).
//!
//! Does not reconstruct `LoopStateMachine` — only aggregates kernel OS events for
//! introspection, tests, and tooling.

use serde::{Deserialize, Serialize};

use crate::runtime::session::SessionEvent;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignalDeliveryDisposedRecord {
    pub turn: u32,
    pub operation_id: String,
    pub delivery_id: String,
    pub attempt: u32,
    pub signal_id: String,
    pub disposition: String,
    pub queue_depth: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessRecord {
    pub turn: u32,
    pub agent_id: String,
    pub parent_session_id: String,
    pub state: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SuspendRecord {
    pub turn: u32,
    pub reason: String,
    pub pending_calls: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetExceededRecord {
    pub turn: u32,
    pub budget: String,
}

/// Aggregated kernel OS state derived from session log (audit view).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OsSnapshot {
    pub last_suspend: Option<SuspendRecord>,
    pub last_resumed_turn: Option<u32>,
    pub process_by_agent: Vec<ProcessRecord>,
    pub budget_exceeded: Vec<BudgetExceededRecord>,
    pub signals: Vec<SignalDeliveryDisposedRecord>,
    pub page_out_count: u32,
    pub page_in_count: u32,
    pub tool_gated_count: u32,
    #[serde(default)]
    pub memory_written_count: u32,
    #[serde(default)]
    pub memory_queried_count: u32,
    #[serde(default)]
    pub memory_validation_failed_count: u32,
    #[serde(default)]
    pub memory_retrieval_result_count: u32,
}

/// Rebuild an OS audit snapshot from session events (newest process state wins per agent).
pub fn rebuild_os_snapshot_from_events(events: &[SessionEvent]) -> OsSnapshot {
    let mut snap = OsSnapshot::default();
    let mut process_index: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();

    for event in events {
        // `is_kernel_os_event` covers every counted OS event; `MemoryRetrievalResult` is an
        // SDK-written acknowledgment counted here too (parity with the node/python rebuilds,
        // which special-case it before their filters).
        if !event.is_kernel_os_event()
            && !matches!(event, SessionEvent::MemoryRetrievalResult { .. })
        {
            continue;
        }

        match event {
            SessionEvent::Suspended {
                turn,
                reason,
                pending_calls,
                ..
            } => {
                snap.last_suspend = Some(SuspendRecord {
                    turn: *turn,
                    reason: reason.clone(),
                    pending_calls: pending_calls.clone(),
                });
            }
            SessionEvent::Resumed { turn, .. } => {
                snap.last_resumed_turn = Some(*turn);
            }
            SessionEvent::ToolGated { .. } => {
                snap.tool_gated_count += 1;
            }
            SessionEvent::AgentProcessChanged {
                turn,
                agent_id,
                parent_session_id,
                state,
                ..
            } => {
                let record = ProcessRecord {
                    turn: *turn,
                    agent_id: agent_id.clone(),
                    parent_session_id: parent_session_id.clone(),
                    state: state.clone(),
                };
                if let Some(idx) = process_index.get(agent_id) {
                    snap.process_by_agent[*idx] = record;
                } else {
                    process_index.insert(agent_id.clone(), snap.process_by_agent.len());
                    snap.process_by_agent.push(record);
                }
            }
            SessionEvent::BudgetExceeded { turn, budget, .. } => {
                snap.budget_exceeded.push(BudgetExceededRecord {
                    turn: *turn,
                    budget: budget.clone(),
                });
            }
            SessionEvent::SignalDeliveryDisposed {
                turn,
                operation_id,
                delivery_id,
                attempt,
                signal_id,
                disposition,
                queue_depth,
                ..
            } => {
                snap.signals.push(SignalDeliveryDisposedRecord {
                    turn: *turn,
                    operation_id: operation_id.clone(),
                    delivery_id: delivery_id.clone(),
                    attempt: *attempt,
                    signal_id: signal_id.clone(),
                    disposition: disposition.clone(),
                    queue_depth: *queue_depth,
                });
            }
            SessionEvent::PageOut { .. } => {
                snap.page_out_count += 1;
            }
            SessionEvent::PageIn { .. } => {
                snap.page_in_count += 1;
            }
            SessionEvent::MemoryWritten { .. } => {
                snap.memory_written_count += 1;
            }
            SessionEvent::MemoryQueried { .. } => {
                snap.memory_queried_count += 1;
            }
            SessionEvent::MemoryValidationFailed { .. } => {
                snap.memory_validation_failed_count += 1;
            }
            SessionEvent::MemoryRetrievalResult { .. } => {
                snap.memory_retrieval_result_count += 1;
            }
            _ => {}
        }
    }

    snap
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rebuild_tracks_process_and_signals() {
        let events = vec![
            SessionEvent::AgentProcessChanged {
                turn: 1,
                agent_id: "child-1".into(),
                parent_session_id: "parent".into(),
                role: "worker".into(),
                isolation: "shared".into(),
                context_inheritance: "none".into(),
                state: "running".into(),
                permitted_capability_ids: vec![],
                result_termination: None,
            },
            SessionEvent::SignalDeliveryDisposed {
                turn: 2,
                operation_id: "op".into(),
                delivery_id: "delivery".into(),
                attempt: 1,
                signal_id: "sig-a".into(),
                disposition: "queue".into(),
                queue_depth: 1,
            },
            SessionEvent::Suspended {
                turn: 3,
                reason: "ask_user".into(),
                pending_calls: vec!["c1".into()],
            },
            SessionEvent::AgentProcessChanged {
                turn: 4,
                agent_id: "child-1".into(),
                parent_session_id: "parent".into(),
                role: "worker".into(),
                isolation: "shared".into(),
                context_inheritance: "none".into(),
                state: "joined".into(),
                permitted_capability_ids: vec![],
                result_termination: Some("completed".into()),
            },
        ];
        let snap = rebuild_os_snapshot_from_events(&events);
        assert_eq!(snap.process_by_agent.len(), 1);
        assert_eq!(snap.process_by_agent[0].state, "joined");
        assert_eq!(snap.signals.len(), 1);
        assert_eq!(snap.last_suspend.as_ref().map(|s| s.reason.as_str()), Some("ask_user"));
    }

    fn load_fixture(name: &str) -> String {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/session")
            .join(name);
        std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {}", path.display(), e))
    }

    fn assert_golden(events_file: &str, snapshot_file: &str) {
        let events: Vec<SessionEvent> =
            serde_json::from_str(&load_fixture(events_file)).expect("events json");
        let snap = rebuild_os_snapshot_from_events(&events);
        let expected: OsSnapshot =
            serde_json::from_str(&load_fixture(snapshot_file)).expect("snapshot json");
        assert_eq!(snap, expected);
    }

    #[test]
    fn golden_os_snapshot_spawn_lifecycle_fixture() {
        assert_golden("events_spawn_lifecycle.json", "os_snapshot_spawn_lifecycle.json");
    }

    #[test]
    fn golden_os_snapshot_ask_user_fixture() {
        assert_golden("events_ask_user.json", "os_snapshot_ask_user.json");
    }
}
