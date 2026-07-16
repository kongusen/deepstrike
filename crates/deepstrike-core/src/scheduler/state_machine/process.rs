//! Sub-agent process lifecycle impl for [`super::LoopStateMachine`].

use super::super::tcb::{TaskLifecycle, TaskTable, Tcb, WaitReason};
use super::{KernelObservation, LoopAction, LoopPhase, LoopStateMachine, SuspendState};
use crate::proc::AgentProcess;
use crate::syscall::{Disposition, Syscall};
use crate::types::result::{SubAgentResult, TerminationReason};
use crate::AgentRunSpec;

impl LoopStateMachine {
    /// Spawn a sub-agent: registers a kernel process, emits `AgentProcessChanged`,
    /// and enters `Suspended(SubAgentAwait)` until the SDK feeds `SubAgentCompleted`.
    pub fn spawn_sub_agent(&mut self, spec: AgentRunSpec, parent_session_id: &str) -> LoopAction {
        let manifest = crate::types::agent::IsolationManifest::from_spec(
            &spec,
            parent_session_id,
            &self.ctx.capabilities,
        );
        // M2b: spawning is an effectful request — route it through the same syscall trap as tool
        // calls. A rejected spawn has not executed, so it is surfaced as a committed control result
        // instead of rolling back the parent transaction.
        if let Disposition::Deny { reason, .. } =
            self.evaluate_syscall(&Syscall::Spawn(manifest.clone()))
        {
            self.observations
                .push(KernelObservation::ControlRequestRejected {
                    turn: self.turn,
                    operation: "spawn_sub_agent".to_string(),
                    subject: Some(manifest.agent_id.to_string()),
                    reason,
                });
            return LoopAction::AwaitingResume;
        }
        let agent_id = manifest.agent_id.to_string();
        // M1 収口: register the sub-agent as a child task — the single source of truth. The
        // `AgentProcess` view row is reconstructed from the TCB for the observation/session-log.
        let child = Tcb::spawned(&manifest, self.policy.clone());
        self.tasks.insert(child);
        if let Some(process) = self.tasks.get(&agent_id).and_then(AgentProcess::from_tcb) {
            self.push_agent_process_changed(process);
        }
        self.suspend_state = Some(SuspendState::SubAgentAwait {
            agent_ids: vec![agent_id.clone()],
        });
        self.set_lifecycle(
            TaskLifecycle::Suspended,
            Some(WaitReason::SubAgentJoin(vec![manifest.agent_id.clone()])),
        );
        self.observations.push(KernelObservation::Suspended {
            turn: self.turn,
            reason: "sub_agent_await".to_string(),
            pending_calls: vec![agent_id],
        });
        LoopAction::AwaitingResume
    }

    pub(super) fn handle_sub_agent_completed(&mut self, result: SubAgentResult) -> LoopAction {
        // M1 収口: record the join on the child task itself (the source of truth) — both the
        // terminal lifecycle and the result payload — then rebuild the `AgentProcess` view row.
        // The terminal `TaskLifecycle` preserves the legacy `ProcessState`→`TaskLifecycle` mapping
        // (`Completed`→`Done(Completed)`, anything else→`Done(Error)`).
        let process = if let Some(task) = self.tasks.get_mut(result.agent_id.as_str()) {
            let process_state = match result.result.termination {
                TerminationReason::Completed => crate::proc::ProcessState::Joined,
                _ => crate::proc::ProcessState::Failed,
            };
            task.state = TaskLifecycle::from(process_state);
            if let Some(info) = task.proc.as_mut() {
                info.result = Some(result.clone());
            }
            AgentProcess::from_tcb(task)
        } else {
            None
        };
        if let Some(process) = process {
            self.push_agent_process_changed(process);
        }
        let summary = result
            .result
            .final_message
            .as_ref()
            .and_then(|m| m.content.as_text())
            .unwrap_or_default();
        // R3-3 cross-boundary provenance: a quarantined node read untrusted content, so its output
        // crossing into the trusted parent context is labeled as untrusted-origin. The kernel
        // enforces the *label* (auditable, machine-checkable); shaping the output into a structured
        // summary stays the SDK's job, since the kernel cannot inspect content shape.
        let quarantined = self
            .workflow
            .as_ref()
            .is_some_and(|w| w.is_agent_quarantined(result.agent_id.as_str()));
        let marker = if quarantined {
            "quarantined sub-agent"
        } else {
            "sub-agent"
        };
        self.ctx
            .push_signal(format!("[{marker} {}] {}", result.agent_id, summary));

        // W0: if a workflow owns this agent, advance its DAG (feed completion, drain the batch,
        // spawn the next gated batch or finish) instead of the single-spawn barrier below.
        if self
            .workflow
            .as_ref()
            .is_some_and(|w| w.owns_agent(result.agent_id.as_str()))
        {
            return self.advance_workflow(result);
        }

        let agent_id = result.agent_id.to_string();
        // Suspended awaiting a sub-agent join (lifecycle on the root task, M1d).
        let awaiting_sub_agent =
            self.is_suspended() && matches!(self.wait_reason(), Some(WaitReason::SubAgentJoin(_)));
        let resume_parent = match self.suspend_state.as_mut() {
            Some(SuspendState::SubAgentAwait { agent_ids }) if awaiting_sub_agent => {
                agent_ids.retain(|id| id != &agent_id);
                if agent_ids.is_empty() {
                    self.suspend_state = None;
                    self.observations.push(KernelObservation::Resumed {
                        turn: self.turn,
                        approved: vec![agent_id],
                        denied: Vec::new(),
                    });
                    true
                } else {
                    false
                }
            }
            _ => true,
        };

        if resume_parent {
            self.phase = LoopPhase::Reason;
            self.emit_call_llm()
        } else {
            LoopAction::AwaitingResume
        }
    }

    /// The `AgentProcess` view of a sub-agent, reconstructed from its child task. `None` for the
    /// root task or unknown ids. (M1 収口: derived from the `TaskTable`, no separate storage.)
    pub fn agent_process(&self, agent_id: &str) -> Option<AgentProcess> {
        self.tasks.get(agent_id).and_then(AgentProcess::from_tcb)
    }

    /// The `AgentProcess` view of all sub-agents (every child task with process identity).
    pub fn agent_processes(&self) -> Vec<AgentProcess> {
        self.tasks
            .all()
            .iter()
            .filter_map(AgentProcess::from_tcb)
            .collect()
    }

    /// The canonical task registry (root task + one row per sub-agent): the single source of
    /// truth for schedulability *and* sub-agent lineage. `agent_process(es)` are derived views
    /// over this table (M1 収口).
    pub fn task_table(&self) -> &TaskTable {
        &self.tasks
    }

    /// Emit an `AgentProcessChanged` observation for a process state transition.
    pub(super) fn push_agent_process_changed(&mut self, process: AgentProcess) {
        // Wire form: role/isolation/inheritance are debug-lowercase (`readonly`, `systemonly`),
        // state via `label()`. Preserved verbatim from the former `From<LoopObservation>` so the
        // observation merge stays byte-identical (locked by `agent_process_changed_locks_*` test).
        self.observations
            .push(KernelObservation::AgentProcessChanged {
                turn: self.turn,
                agent_id: process.agent_id.to_string(),
                parent_session_id: process.parent_session_id.to_string(),
                role: format!("{:?}", process.role).to_lowercase(),
                isolation: format!("{:?}", process.isolation).to_lowercase(),
                context_inheritance: format!("{:?}", process.context_inheritance).to_lowercase(),
                state: process.state.label().to_string(),
                permitted_capability_ids: process
                    .permitted_capability_ids
                    .iter()
                    .map(|id| id.to_string())
                    .collect(),
                result_termination: process.result_termination_label().map(str::to_string),
            });
    }
}
