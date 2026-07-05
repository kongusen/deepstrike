use compact_str::CompactString;
use serde::{Deserialize, Serialize};

use crate::types::agent::{AgentIsolation, AgentRole, ContextInheritance};
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
/// `AgentProcess` view. Inverse of `impl From<ProcessState> for TaskLifecycle`: a child task is
/// `Joined` once it completed successfully, `Failed` on any other terminal reason, else `Running`.
fn process_state_of(state: crate::scheduler::tcb::TaskLifecycle) -> ProcessState {
    use crate::scheduler::tcb::TaskLifecycle;
    match state {
        TaskLifecycle::Done(TerminationReason::Completed) => ProcessState::Joined,
        TaskLifecycle::Done(_) => ProcessState::Failed,
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
    /// Reconstruct an `AgentProcess` from a child [`crate::scheduler::tcb::Tcb`] (M1 收口).
    ///
    /// Returns `None` for the root task (no `proc`). This is the bridge that makes the
    /// `AgentProcess` records a *derived view* over the kernel's `TaskTable`: the sub-agent's
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
        Some(self.result.as_ref()?.result.termination.label())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scheduler::policy::SchedulerBudget;
    use crate::scheduler::tcb::{Tcb, TaskLifecycle};
    use crate::types::agent::{AgentIdentity, AgentRole, AgentRunSpec, IsolationManifest};
    use crate::types::capability::CapabilityManifest;

    fn child_tcb(id: &str) -> Tcb {
        let spec = AgentRunSpec::new(
            AgentIdentity::sub_agent(id, &format!("{id}-session")),
            AgentRole::Implement,
            "do work",
        );
        let manifest = IsolationManifest::from_spec(&spec, "parent-sess", &CapabilityManifest::new());
        Tcb::spawned(&manifest, SchedulerBudget::default())
    }

    #[test]
    fn from_tcb_is_none_for_root_task() {
        let root = Tcb::root("root", SchedulerBudget::default());
        assert!(AgentProcess::from_tcb(&root).is_none());
    }

    #[test]
    fn from_tcb_reconstructs_running_process() {
        let tcb = child_tcb("worker");
        let p = AgentProcess::from_tcb(&tcb).expect("child reconstructs a process");
        assert_eq!(p.agent_id.as_str(), "worker");
        assert_eq!(p.parent_session_id.as_str(), "parent-sess");
        assert_eq!(p.role, AgentRole::Implement);
        assert_eq!(p.state, ProcessState::Running);
        assert!(p.result.is_none());
    }

    #[test]
    fn process_state_of_maps_terminal_task_states() {
        assert_eq!(process_state_of(TaskLifecycle::Running), ProcessState::Running);
        assert_eq!(
            process_state_of(TaskLifecycle::Done(TerminationReason::Completed)),
            ProcessState::Joined
        );
        assert_eq!(
            process_state_of(TaskLifecycle::Done(TerminationReason::Error)),
            ProcessState::Failed
        );
        assert_eq!(
            process_state_of(TaskLifecycle::Done(TerminationReason::Timeout)),
            ProcessState::Failed
        );
    }
}
