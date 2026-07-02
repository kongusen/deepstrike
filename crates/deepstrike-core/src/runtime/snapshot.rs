//! W2-2: First-class `KernelSnapshot` — live kernel state serialization.
//!
//! Enables:
//! - Crash recovery (save state, restart, restore)
//! - Agent migration (move running agent between hosts)
//! - Checkpointing for long-running tasks
//! - Cross-session state persistence
//!
//! The snapshot captures the essential kernel state needed to resume execution:
//! - TaskTable (all TCBs with budget, wait state, proc info)
//! - Context summary (messages, task state, signals)
//! - Turn counter and budget totals
//!
//! NOT captured (recreated on restore):
//! - SignalRouter dedup set (cleared on restore)
//! - Governance pipeline closures (recreated from policy data)
//! - MilestoneTracker state (recreated from config)
//! - HandleTable (recreated from context history)

use serde::{Deserialize, Serialize};

use crate::scheduler::tcb::{TaskId, TaskState, Tcb, TaskTable};
use crate::types::agent::{AgentIsolation, AgentRole, ContextInheritance};
use crate::types::message::Message;

/// Serializable snapshot of a TCB (Task Control Block).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TcbSnapshot {
    pub id: TaskId,
    pub parent: Option<TaskId>,
    pub state: TaskState,
    pub turns: u32,
    pub total_tokens: u64,
    pub started_at_ms: Option<u64>,
    pub max_tokens: u32,
    pub max_turns: u32,
    pub max_total_tokens: u64,
    pub max_wall_ms: Option<u64>,
    pub wait_reason: Option<String>,  // Serialized as string label
    pub wait_children: Option<Vec<TaskId>>,  // For SubAgentJoin
    pub deferred_until: Option<u64>,
    pub caps: Vec<TaskId>,
    pub proc: Option<ProcInfoSnapshot>,
}

/// Snapshot of ProcInfo (sub-agent identity).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcInfoSnapshot {
    pub parent_session_id: TaskId,
    pub role: AgentRole,
    pub isolation: AgentIsolation,
    pub context_inheritance: ContextInheritance,
    pub result: Option<ResultSnapshot>,
}

/// Snapshot of SubAgentResult.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResultSnapshot {
    pub termination: String,
}

/// K1: per-entry knowledge identity, parallel to `knowledge_messages` by index. Absent/short
/// vectors (old snapshots) restore as unkeyed/unpinned — graceful, additive.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct KnowledgeEntryMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pinned: bool,
}

/// Snapshot of context state (simplified representation).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ContextSnapshot {
    pub system_messages: Vec<Message>,
    pub knowledge_messages: Vec<Message>,
    /// K1: identity metadata for `knowledge_messages` (index-parallel). `#[serde(default)]` keeps
    /// pre-K1 snapshots loadable; pending upserts/eviction marks are deliberately NOT snapshotted
    /// (graceful reset, same philosophy as `frozen_history_len`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub knowledge_entries_meta: Vec<KnowledgeEntryMeta>,
    pub task_goal: Option<String>,
    pub task_plan: Option<String>,
    pub task_progress: Option<String>,
    pub task_open_steps: Vec<String>,
    /// Durable user directives — preserved across snapshot/restore like goal/plan.
    #[serde(default)]
    pub task_directives: Vec<String>,
    pub history_messages: Vec<Message>,
    pub signals: Vec<String>,
    pub max_tokens: u32,
    pub sprint: u32,
}

impl ContextSnapshot {
    /// Create a snapshot from context manager state.
    pub fn from_context(ctx: &crate::context::manager::ContextManager) -> Self {
        // Convert plan steps to JSON string representation
        let task_plan = if ctx.partitions.task_state.plan.is_empty() {
            None
        } else {
            serde_json::to_string(&ctx.partitions.task_state.plan).ok()
        };

        Self {
            system_messages: ctx.partitions.system.messages.clone(),
            knowledge_messages: ctx.partitions.knowledge.messages().cloned().collect(),
            knowledge_entries_meta: ctx
                .partitions
                .knowledge
                .entries
                .iter()
                .map(|e| KnowledgeEntryMeta {
                    key: e.key.as_ref().map(|k| k.to_string()),
                    pinned: e.pinned,
                })
                .collect(),
            task_goal: Some(ctx.partitions.task_state.goal.clone()),
            task_plan,
            task_progress: Some(ctx.partitions.task_state.progress.clone()),
            task_open_steps: ctx.partitions.task_state.open_steps(),
            task_directives: ctx.partitions.task_state.directives.clone(),
            history_messages: ctx.partitions.history.messages.clone(),
            signals: ctx.partitions.signals.clone(),
            max_tokens: ctx.max_tokens,
            sprint: ctx.sprint,
        }
    }
}

/// Full kernel snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KernelSnapshot {
    pub turn: u32,
    pub total_tokens: u64,
    pub tasks: Vec<TcbSnapshot>,
    pub context: ContextSnapshot,
    pub run_spec: Option<String>,  // JSON-encoded AgentRunSpec
}

impl KernelSnapshot {
    /// Create a snapshot from kernel state components.
    pub fn from_state(
        turn: u32,
        total_tokens: u64,
        tasks: &TaskTable,
        context: &ContextSnapshot,
        run_spec: Option<&crate::AgentRunSpec>,
    ) -> Self {
        Self {
            turn,
            total_tokens,
            tasks: tasks.all().iter().map(TcbSnapshot::from).collect(),
            context: context.clone(),
            run_spec: run_spec.and_then(|s| serde_json::to_string(s).ok()),
        }
    }

    /// Convert back to AgentRunSpec if present.
    pub fn run_spec(&self) -> Option<crate::AgentRunSpec> {
        self.run_spec.as_ref().and_then(|s| serde_json::from_str(s).ok())
    }

    /// W2-2: Convert to OsSnapshot-compatible process records for verification with
    /// `rebuild_os_snapshot_from_events`. This enables cross-validation between the
    /// live state snapshot and the event-log-derived audit view.
    pub fn to_os_process_records(&self) -> Vec<crate::runtime::replay::ProcessRecord> {
        use crate::runtime::replay::ProcessRecord;
        let mut records = Vec::new();
        for tcb_snap in &self.tasks {
            // Only include tasks with ProcInfo (sub-agents), not the root task
            if let Some(proc) = &tcb_snap.proc {
                records.push(ProcessRecord {
                    turn: tcb_snap.turns,
                    agent_id: tcb_snap.id.to_string(),
                    parent_session_id: proc.parent_session_id.to_string(),
                    state: tcb_snap.state.label().to_string(),
                });
            }
        }
        records
    }

    /// W2-2: Restore TCB from snapshot. Returns None if the snapshot data is invalid.
    pub fn restore_tcb(&self, snapshot: &TcbSnapshot) -> Option<Tcb> {
        use crate::scheduler::policy::SchedulerBudget;

        // Reconstruct BudgetLedger limits
        let limits = SchedulerBudget {
            max_tokens: snapshot.max_tokens,
            max_turns: snapshot.max_turns,
            max_total_tokens: snapshot.max_total_tokens,
            max_wall_ms: snapshot.max_wall_ms,
        };

        // Reconstruct wait reason from label
        let wait = snapshot.wait_reason.as_ref().and_then(|label| match label.as_str() {
            "approval" => Some(crate::scheduler::tcb::WaitReason::Approval),
            "sub_agent_join" => snapshot.wait_children.as_ref().map(|children| {
                crate::scheduler::tcb::WaitReason::SubAgentJoin(
                    children.iter().map(|id| id.clone().into()).collect()
                )
            }),
            "tool" => Some(crate::scheduler::tcb::WaitReason::Tool),
            "milestone" => Some(crate::scheduler::tcb::WaitReason::Milestone),
            "signal" => Some(crate::scheduler::tcb::WaitReason::Signal),
            "external" => Some(crate::scheduler::tcb::WaitReason::External),
            _ => None,
        });

        // Reconstruct ProcInfo if present
        let proc = snapshot.proc.as_ref().and_then(|p| {
            let result = p.result.as_ref().and_then(|r| {
                // Parse termination string back to TerminationReason
                match r.termination.as_str() {
                    "\"Completed\"" | "Completed" => Some(crate::types::result::SubAgentResult {
                        agent_id: snapshot.id.clone(),
                        result: crate::types::result::LoopResult {
                            termination: crate::types::result::TerminationReason::Completed,
                            final_message: None,
                            turns_used: 0,
                            total_tokens_used: 0,
                            loop_continue: None,
                            classify_branch: None,
                            tournament_winner: None,
                        },
                    }),
                    _ => None,
                }
            });

            Some(crate::scheduler::tcb::ProcInfo {
                parent_session_id: p.parent_session_id.clone(),
                role: p.role,
                isolation: p.isolation,
                context_inheritance: p.context_inheritance,
                result,
            })
        });

        Some(Tcb {
            id: snapshot.id.clone(),
            parent: snapshot.parent.clone(),
            state: snapshot.state,
            budget: crate::scheduler::tcb::BudgetLedger {
                limits,
                turns: snapshot.turns,
                total_tokens: snapshot.total_tokens,
                started_at_ms: snapshot.started_at_ms,
            },
            wait,
            caps: snapshot.caps.clone(),
            proc,
            deferred_until: snapshot.deferred_until,
        })
    }
}

impl From<&Tcb> for TcbSnapshot {
    fn from(tcb: &Tcb) -> Self {
        Self {
            id: tcb.id.clone(),
            parent: tcb.parent.clone(),
            state: tcb.state.clone(),
            turns: tcb.budget.turns,
            total_tokens: tcb.budget.total_tokens,
            started_at_ms: tcb.budget.started_at_ms,
            max_tokens: tcb.budget.limits.max_tokens,
            max_turns: tcb.budget.limits.max_turns,
            max_total_tokens: tcb.budget.limits.max_total_tokens,
            max_wall_ms: tcb.budget.limits.max_wall_ms,
            wait_reason: tcb.wait.as_ref().map(|w| w.label().to_string()),
            wait_children: match &tcb.wait {
                Some(crate::scheduler::tcb::WaitReason::SubAgentJoin(children)) => {
                    Some(children.clone())
                }
                _ => None,
            },
            deferred_until: tcb.deferred_until,
            caps: tcb.caps.clone(),
            proc: tcb.proc.as_ref().map(|p| ProcInfoSnapshot {
                parent_session_id: p.parent_session_id.clone(),
                role: p.role,
                isolation: p.isolation,
                context_inheritance: p.context_inheritance,
                result: p.result.as_ref().map(|r| ResultSnapshot {
                    termination: format!("{:?}", r.result.termination),
                }),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scheduler::policy::SchedulerBudget;

    #[test]
    fn tcb_snapshot_roundtrip() {
        let mut tcb = Tcb::root("test-task", SchedulerBudget {
            max_tokens: 128_000,
            max_turns: 10,
            max_total_tokens: 1000,
            max_wall_ms: Some(60000),
        });
        tcb.budget.turns = 5;
        tcb.budget.total_tokens = 500;
        tcb.deferred_until = Some(1000);

        let snapshot = TcbSnapshot::from(&tcb);
        assert_eq!(snapshot.id.as_str(), "test-task");
        assert_eq!(snapshot.turns, 5);
        assert_eq!(snapshot.total_tokens, 500);
        assert_eq!(snapshot.deferred_until, Some(1000));
    }

    #[test]
    fn kernel_snapshot_serializes() {
        let snap = KernelSnapshot {
            turn: 1,
            total_tokens: 100,
            tasks: vec![],
            context: ContextSnapshot::default(),
            run_spec: None,
        };

        let json = serde_json::to_string(&snap).expect("serialize");
        let restored: KernelSnapshot = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(restored.turn, 1);
        assert_eq!(restored.total_tokens, 100);
    }

    #[test]
    fn context_snapshot_captures_fields() {
        let ctx = ContextSnapshot {
            system_messages: vec![Message::system("You are helpful")],
            task_goal: Some("Build something".to_string()),
            ..Default::default()
        };

        assert_eq!(ctx.system_messages.len(), 1);
        assert_eq!(ctx.task_goal.as_deref(), Some("Build something"));
    }

    #[test]
    fn snapshot_from_state_captures_tasks() {
        let mut table = TaskTable::new();
        table.insert(Tcb::root("root", SchedulerBudget::default()));
        table.insert(Tcb::root("child", SchedulerBudget::default()));

        let ctx = ContextSnapshot::default();
        let snap = KernelSnapshot::from_state(5, 1000, &table, &ctx, None);

        assert_eq!(snap.turn, 5);
        assert_eq!(snap.total_tokens, 1000);
        assert_eq!(snap.tasks.len(), 2);
    }

    #[test]
    fn context_snapshot_from_manager() {
        use crate::context::manager::ContextManager;
        use crate::types::message::Message;

        let mut ctx = ContextManager::new(1000);
        ctx.partitions.system.push(Message::system("You are helpful"), 10);
        ctx.partitions.task_state.goal = "Test goal".to_string();

        let snap = ContextSnapshot::from_context(&ctx);
        assert_eq!(snap.system_messages.len(), 1);
        assert_eq!(snap.task_goal.as_deref(), Some("Test goal"));
        assert_eq!(snap.max_tokens, 1000);
        // Empty plan becomes None
        assert!(snap.task_plan.is_none());
    }

    #[test]
    fn tcb_restore_roundtrip() {
        let original = Tcb::root("test-task", SchedulerBudget {
            max_tokens: 128_000,
            max_turns: 10,
            max_total_tokens: 1000,
            max_wall_ms: Some(60000),
        });

        let snap = TcbSnapshot::from(&original);
        let kernel_snap = KernelSnapshot {
            turn: 1,
            total_tokens: 100,
            tasks: vec![snap.clone()],
            context: ContextSnapshot::default(),
            run_spec: None,
        };

        let restored = kernel_snap.restore_tcb(&snap);
        assert!(restored.is_some());

        let tcb = restored.unwrap();
        assert_eq!(tcb.id.as_str(), "test-task");
        assert_eq!(tcb.state, original.state);
        assert_eq!(tcb.budget.turns, original.budget.turns);
    }

    #[test]
    fn tcb_restore_with_wait_reason() {
        let mut tcb = Tcb::root("waiting-task", SchedulerBudget::default());
        tcb.wait = Some(crate::scheduler::tcb::WaitReason::SubAgentJoin(
            vec!["child-1".into(), "child-2".into()]
        ));

        let snap = TcbSnapshot::from(&tcb);
        let kernel_snap = KernelSnapshot {
            turn: 1,
            total_tokens: 100,
            tasks: vec![snap.clone()],
            context: ContextSnapshot::default(),
            run_spec: None,
        };

        let restored = kernel_snap.restore_tcb(&snap).expect("restore should succeed");
        match restored.wait {
            Some(crate::scheduler::tcb::WaitReason::SubAgentJoin(children)) => {
                assert_eq!(children.len(), 2);
                assert_eq!(children[0].as_str(), "child-1");
                assert_eq!(children[1].as_str(), "child-2");
            }
            other => panic!("Expected SubAgentJoin, got {:?}", other),
        }
    }

    #[test]
    fn kernel_snapshot_to_os_process_records() {
        // Create a snapshot with root + sub-agent tasks
        let snap = KernelSnapshot {
            turn: 5,
            total_tokens: 1000,
            tasks: vec![
                // Root task (no ProcInfo - should be filtered out)
                TcbSnapshot {
                    id: "root".into(),
                    parent: None,
                    state: TaskState::Running,
                    turns: 5,
                    total_tokens: 500,
                    started_at_ms: Some(0),
                    max_tokens: 128_000,
                    max_turns: 100,
                    max_total_tokens: 1_000_000,
                    max_wall_ms: None,
                    wait_reason: None,
                    wait_children: None,
                    deferred_until: None,
                    caps: vec![],
                    proc: None,
                },
                // Sub-agent task (has ProcInfo - should be included)
                TcbSnapshot {
                    id: "child-1".into(),
                    parent: Some("root".into()),
                    state: TaskState::Done(crate::types::result::TerminationReason::Completed),
                    turns: 3,
                    total_tokens: 300,
                    started_at_ms: Some(100),
                    max_tokens: 64_000,
                    max_turns: 50,
                    max_total_tokens: 500_000,
                    max_wall_ms: None,
                    wait_reason: None,
                    wait_children: None,
                    deferred_until: None,
                    caps: vec![],
                    proc: Some(ProcInfoSnapshot {
                        parent_session_id: "root".into(),
                        role: AgentRole::Implement,
                        isolation: AgentIsolation::Shared,
                        context_inheritance: ContextInheritance::None,
                        result: None,
                    }),
                },
            ],
            context: ContextSnapshot::default(),
            run_spec: None,
        };

        let records = snap.to_os_process_records();
        assert_eq!(records.len(), 1); // Root task filtered out
        assert_eq!(records[0].agent_id, "child-1");
        assert_eq!(records[0].parent_session_id, "root");
        assert_eq!(records[0].turn, 3);
        assert_eq!(records[0].state, "done"); // Done state -> "done" label
    }

    #[test]
    fn kernel_snapshot_to_os_records_matches_state_machine() {
        use crate::scheduler::state_machine::LoopStateMachine;
        use crate::scheduler::policy::LoopPolicy;
        use crate::types::agent::{AgentIdentity, AgentRole, AgentRunSpec};

        // Create a state machine and spawn a sub-agent
        let mut sm = LoopStateMachine::new(LoopPolicy {
            max_tokens: 128_000,
            ..Default::default()
        });
        sm.start(crate::types::task::RuntimeTask::new("parent task"));

        // Spawn sub-agent
        let _ = sm.spawn_sub_agent(
            AgentRunSpec::new(
                AgentIdentity::sub_agent("child", "child-session"),
                AgentRole::Implement,
                "child task",
            ),
            "parent-sess",
        );

        // Take snapshot and convert to OsSnapshot records
        let snap = sm.snapshot();
        let records = snap.to_os_process_records();

        // Should have exactly one sub-agent record
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].agent_id, "child");
        assert_eq!(records[0].parent_session_id, "parent-sess");
        // State should be "running" or "suspended" (depends on when snapshot was taken)
        assert!(
            records[0].state == "running" || records[0].state == "suspended",
            "unexpected state: {}",
            records[0].state
        );
    }

    #[test]
    fn pre_k1_snapshot_without_entries_meta_still_loads() {
        // K1 back-compat: a snapshot serialized before `knowledge_entries_meta` existed must
        // deserialize (serde default) and restore every knowledge entry unkeyed/unpinned.
        let json = serde_json::json!({
            "system_messages": [],
            "knowledge_messages": [
                { "role": "system", "content": "legacy knowledge" }
            ],
            "task_goal": "g",
            "task_plan": null,
            "task_progress": "p",
            "task_open_steps": [],
            "history_messages": [],
            "signals": [],
            "max_tokens": 1000,
            "sprint": 0
        });
        let snap: ContextSnapshot = serde_json::from_value(json).expect("old snapshot loads");
        assert!(snap.knowledge_entries_meta.is_empty());

        let kernel_snap = KernelSnapshot {
            turn: 0,
            total_tokens: 0,
            tasks: vec![],
            context: snap,
            run_spec: None,
        };
        let sm = crate::scheduler::state_machine::LoopStateMachine::restore(&kernel_snap);
        assert_eq!(sm.ctx.partitions.knowledge.len(), 1);
        let entry = &sm.ctx.partitions.knowledge.entries[0];
        assert!(entry.key.is_none());
        assert!(!entry.pinned);
    }

    #[test]
    fn keyed_pinned_knowledge_round_trips_through_snapshot() {
        // K1: key + pinned survive snapshot → restore (index-parallel meta vec).
        let mut ctx = crate::context::manager::ContextManager::new(1000);
        ctx.push_knowledge_entry(Some("ref".into()), Message::system("keyed doc"), 5, true);
        ctx.push_knowledge(Message::system("legacy"), 3);

        let context_snap = ContextSnapshot::from_context(&ctx);
        assert_eq!(context_snap.knowledge_entries_meta.len(), 2);

        let kernel_snap = KernelSnapshot {
            turn: 0,
            total_tokens: 0,
            tasks: vec![],
            context: context_snap,
            run_spec: None,
        };
        let sm = crate::scheduler::state_machine::LoopStateMachine::restore(&kernel_snap);
        let entries = &sm.ctx.partitions.knowledge.entries;
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].key.as_deref(), Some("ref"));
        assert!(entries[0].pinned);
        assert!(entries[1].key.is_none());
        assert!(!entries[1].pinned);
    }
}
