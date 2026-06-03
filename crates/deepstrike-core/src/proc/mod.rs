use compact_str::CompactString;
use serde::{Deserialize, Serialize};

use crate::types::agent::{AgentIsolation, AgentRole, ContextInheritance, IsolationManifest};
use crate::types::result::{SubAgentResult, TerminationReason};

/// Kernel-owned lifecycle state for a spawned agent process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessState {
    Running,
    Joined,
    Failed,
}

impl ProcessState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Joined => "joined",
            Self::Failed => "failed",
        }
    }
}

/// Project a task's schedulability onto the coarser process lifecycle exposed in the
/// `AgentProcess` view. Inverse of `impl From<ProcessState> for TaskState`: a child task is
/// `Joined` once it completed successfully, `Failed` on any other terminal reason, else `Running`.
fn process_state_of(state: crate::scheduler::tcb::TaskState) -> ProcessState {
    use crate::scheduler::tcb::TaskState;
    match state {
        TaskState::Done(TerminationReason::Completed) => ProcessState::Joined,
        TaskState::Done(_) => ProcessState::Failed,
        _ => ProcessState::Running,
    }
}

/// A sub-agent process registered by the kernel.
///
/// The kernel owns only declarative lifecycle state. Host execution,
/// worktree/remote isolation, I/O, and concurrency remain SDK concerns.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentProcess {
    pub agent_id: CompactString,
    pub parent_session_id: CompactString,
    pub role: AgentRole,
    pub isolation: AgentIsolation,
    pub context_inheritance: ContextInheritance,
    pub state: ProcessState,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub permitted_capability_ids: Vec<CompactString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<SubAgentResult>,
}

impl AgentProcess {
    pub fn from_manifest(manifest: &IsolationManifest) -> Self {
        Self {
            agent_id: manifest.agent_id.clone(),
            parent_session_id: manifest.parent_session_id.clone(),
            role: manifest.role,
            isolation: manifest.isolation,
            context_inheritance: manifest.context_inheritance,
            state: ProcessState::Running,
            permitted_capability_ids: manifest.permitted_capability_ids.clone(),
            result: None,
        }
    }

    pub fn complete(&mut self, result: SubAgentResult) {
        self.state = match result.result.termination {
            TerminationReason::Completed => ProcessState::Joined,
            _ => ProcessState::Failed,
        };
        self.result = Some(result);
    }

    /// Reconstruct an `AgentProcess` from a child [`crate::scheduler::tcb::Tcb`] (M1 收口).
    ///
    /// Returns `None` for the root task (no `proc`). This is the bridge that makes the
    /// `ProcessTable` a *derived view* over the kernel's `TaskTable`: the sub-agent's
    /// declarative identity lives on the TCB, and the `AgentProcess` shape — the SDK ABI /
    /// session-log contract — is rebuilt on demand without a second source of truth.
    pub fn from_tcb(tcb: &crate::scheduler::tcb::Tcb) -> Option<Self> {
        let info = tcb.proc.as_ref()?;
        Some(Self {
            agent_id: tcb.id.clone(),
            parent_session_id: info.parent_session_id.clone(),
            role: info.role,
            isolation: info.isolation,
            context_inheritance: info.context_inheritance,
            state: process_state_of(tcb.state),
            permitted_capability_ids: tcb.caps.clone(),
            result: info.result.clone(),
        })
    }

    pub fn result_termination_label(&self) -> Option<&'static str> {
        let result = self.result.as_ref()?;
        Some(match result.result.termination {
            TerminationReason::Completed => "completed",
            TerminationReason::MaxTurns => "max_turns",
            TerminationReason::TokenBudget => "token_budget",
            TerminationReason::Timeout => "timeout",
            TerminationReason::UserAbort => "user_abort",
            TerminationReason::Error => "error",
            TerminationReason::MilestoneExceeded => "milestone_exceeded",
        })
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProcessTable {
    processes: Vec<AgentProcess>,
}

impl ProcessTable {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register_spawn(&mut self, manifest: &IsolationManifest) -> AgentProcess {
        let process = AgentProcess::from_manifest(manifest);
        if let Some(existing) = self
            .processes
            .iter_mut()
            .find(|p| p.agent_id == process.agent_id)
        {
            *existing = process.clone();
        } else {
            self.processes.push(process.clone());
        }
        process
    }

    pub fn complete(&mut self, result: SubAgentResult) -> Option<AgentProcess> {
        let process = self
            .processes
            .iter_mut()
            .find(|p| p.agent_id == result.agent_id)?;
        process.complete(result);
        Some(process.clone())
    }

    pub fn get(&self, agent_id: &str) -> Option<&AgentProcess> {
        self.processes.iter().find(|p| p.agent_id.as_str() == agent_id)
    }

    pub fn all(&self) -> &[AgentProcess] {
        &self.processes
    }

    /// Child processes registered under a parent session id (lineage audit).
    pub fn children_of(&self, parent_session_id: &str) -> Vec<&AgentProcess> {
        self.processes
            .iter()
            .filter(|p| p.parent_session_id.as_str() == parent_session_id)
            .collect()
    }

    /// Agent ids still in the `Running` state.
    pub fn running_agent_ids(&self) -> Vec<&str> {
        self.processes
            .iter()
            .filter(|p| p.state == ProcessState::Running)
            .map(|p| p.agent_id.as_str())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::agent::{AgentRole, AgentRunSpec};
    use crate::types::agent::AgentIdentity;
    use crate::types::capability::CapabilityManifest;
    use crate::types::message::Message;
    use crate::types::result::{LoopResult, SubAgentResult};

    #[test]
    fn complete_marks_successful_process_joined() {
        let spec = AgentRunSpec::new(
            AgentIdentity::sub_agent("worker", "worker-session"),
            AgentRole::Implement,
            "do work",
        );
        let manifest = IsolationManifest::from_spec(&spec, "parent", &CapabilityManifest::new());
        let mut table = ProcessTable::new();
        table.register_spawn(&manifest);

        table.complete(SubAgentResult {
            agent_id: "worker".into(),
            result: LoopResult {
                termination: TerminationReason::Completed,
                final_message: Some(Message::assistant("done")),
                turns_used: 1,
                total_tokens_used: 10,
            },
        });

        let process = table.get("worker").expect("process");
        assert_eq!(process.state, ProcessState::Joined);
        assert_eq!(process.result_termination_label(), Some("completed"));
    }

    #[test]
    fn failed_join_marks_process_failed() {
        let spec = AgentRunSpec::new(
            AgentIdentity::sub_agent("worker", "worker-session"),
            AgentRole::Implement,
            "do work",
        );
        let manifest = IsolationManifest::from_spec(&spec, "parent", &CapabilityManifest::new());
        let mut table = ProcessTable::new();
        table.register_spawn(&manifest);

        table.complete(SubAgentResult {
            agent_id: "worker".into(),
            result: LoopResult {
                termination: TerminationReason::Error,
                final_message: None,
                turns_used: 1,
                total_tokens_used: 0,
            },
        });

        let process = table.get("worker").expect("process");
        assert_eq!(process.state, ProcessState::Failed);
        assert_eq!(process.result_termination_label(), Some("error"));
    }

    #[test]
    fn children_of_lists_lineage() {
        let mut table = ProcessTable::new();
        for id in ["a", "b"] {
            let spec = AgentRunSpec::new(
                AgentIdentity::sub_agent(id, &format!("{id}-session")),
                AgentRole::Explore,
                "task",
            );
            let manifest = IsolationManifest::from_spec(&spec, "parent-sess", &CapabilityManifest::new());
            table.register_spawn(&manifest);
        }
        assert_eq!(table.children_of("parent-sess").len(), 2);
        assert_eq!(table.running_agent_ids().len(), 2);
    }
}
