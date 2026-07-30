//! Primitive P2: Task Control Block + unified scheduling entity.
//!
//! See `.local-docs/specs/agent-os-three-primitives.md`. The root loop and every
//! sub-agent are a single [`Tcb`]; the [`TaskTable`] is the sole source of truth for
//! schedulability and lineage, and the `AgentProcess` view is derived from it.
//! [`budget_verdict`] is the single budget decision point (turn/token/wall axes),
//! delegating to [`SchedulerBudget::should_terminate`] via [`BudgetLedger`].

use compact_str::CompactString;
use serde::{Deserialize, Serialize};

use crate::proc::ProcessState;
use crate::scheduler::policy::SchedulerBudget;
use crate::types::agent::{AgentIsolation, AgentRole, ContextInheritance, IsolationManifest};
use crate::types::result::{SubAgentResult, TerminationReason};

/// Identity of a schedulable task. Task 0 is the root loop; children are sub-agents.
/// Aligns with `AgentProcess.agent_id` so process rows map onto TCBs 1:1.
pub type TaskId = CompactString;

/// Schedulability lifecycle of a task — orthogonal to the *intra-turn* step,
/// which stays on [`crate::scheduler::state_machine::LoopPhase`], and distinct from
/// the task-goal blackboard [`crate::context::task_state::TaskState`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskLifecycle {
    /// Spec §10.4 · the kernel has minted this task's identity (task/attempt/launch token) but the
    /// launch effect that carries it is not published yet. A child exists as a committed fact
    /// before any host is asked to start it — that is what stops a resolution from *creating*
    /// process identity.
    ///
    /// Only the canonical, ack-gated spawn path constructs this state
    /// ([`crate::scheduler::state_machine::LoopStateMachine::set_ack_gated_launch`]); the legacy
    /// path still seeds children `Running`.
    PendingLaunch,
    /// Spec §10.4 · the launch effect is published and the host has been asked to start the child,
    /// but no acknowledgement has arrived. Distinct from [`Self::Running`] because "we asked" and
    /// "it is running" are different facts, and only the second may be reported as one.
    Starting,
    /// Eligible to run, not yet picked by the scheduler.
    Ready,
    /// Currently executing a turn (`ProcessState::Running`).
    Running,
    /// Suspended awaiting external resolution (human approval / sub-agent join).
    Suspended,
    /// Finished. Carries the termination reason (`ProcessState::{Joined,Failed}`).
    Done(TerminationReason),
}

impl TaskLifecycle {
    pub fn label(self) -> &'static str {
        match self {
            Self::PendingLaunch => "pending_launch",
            Self::Starting => "starting",
            Self::Ready => "ready",
            Self::Running => "running",
            Self::Suspended => "suspended",
            Self::Done(_) => "done",
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Done(_))
    }

    /// Whether this task holds one of the operation's concurrency slots.
    ///
    /// A launch the kernel has already decided on occupies a slot even before the host confirms it
    /// — otherwise every in-flight launch would be invisible to the spawn quota and an ack-gated
    /// run could overshoot `max_concurrent_subagents` by the size of its launch window. Legacy runs
    /// never construct the two launch states, so this reads exactly as `== Running` for them.
    pub fn occupies_slot(self) -> bool {
        matches!(self, Self::PendingLaunch | Self::Starting | Self::Running)
    }
}

/// A successful join maps to `Done(Completed)`; any other termination is `Done(<reason>)`.
impl From<ProcessState> for TaskLifecycle {
    fn from(state: ProcessState) -> Self {
        match state {
            ProcessState::Running => TaskLifecycle::Running,
            ProcessState::Joined => TaskLifecycle::Done(TerminationReason::Completed),
            // Failed has no single reason at the process level; the real reason travels
            // in `SubAgentResult`. This projection maps to a generic error.
            ProcessState::Failed => TaskLifecycle::Done(TerminationReason::Error),
        }
    }
}

/// Why a suspended task is not runnable. Only the reasons production actually
/// constructs exist; new wait states earn a variant when they earn a producer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitReason {
    /// Governance `AskUser` — waiting for SDK to resolve human approval.
    Approval,
    /// Parent blocked on child tasks' join results. Tracks pending child IDs.
    SubAgentJoin(Vec<TaskId>),
}

impl WaitReason {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Approval => "approval",
            Self::SubAgentJoin(_) => "sub_agent_join",
        }
    }
}

/// Running budget counters + limits for a task. Wraps the existing [`SchedulerBudget`]
/// limits so budget evaluation lives here without changing the axes.
#[derive(Debug, Clone)]
pub struct BudgetLedger {
    pub limits: SchedulerBudget,
    pub turns: u32,
    pub total_tokens: u64,
    pub started_at_ms: Option<u64>,
}

impl BudgetLedger {
    pub fn new(limits: SchedulerBudget) -> Self {
        Self {
            limits,
            turns: 0,
            total_tokens: 0,
            started_at_ms: None,
        }
    }

    /// Delegates to the existing budget logic — single source of truth, no axis drift.
    pub fn exceeded(&self, now_ms: Option<u64>) -> Option<&'static str> {
        self.limits
            .should_terminate(self.turns, self.total_tokens, now_ms, self.started_at_ms)
    }
}

impl Default for BudgetLedger {
    fn default() -> Self {
        Self::new(SchedulerBudget::default())
    }
}

/// Sub-agent-specific identity carried by a child [`Tcb`]; `None` on the root task.
///
/// This is what makes the `AgentProcess` view *derived* from the [`TaskTable`]: every child task
/// whose `proc` is `Some` reconstructs exactly one [`crate::proc::AgentProcess`] (see
/// [`crate::proc::AgentProcess::from_tcb`]).
///
/// § Task 11 · carries no session. A child's lineage is the logical `Tcb::parent` task id; the host
/// keeps its own child-task → child-session mapping (§5.2) and the kernel never sees it.
#[derive(Debug, Clone)]
pub struct ProcInfo {
    pub role: AgentRole,
    pub isolation: AgentIsolation,
    pub context_inheritance: ContextInheritance,
    /// The join result once the sub-agent has completed; `None` while running.
    pub result: Option<SubAgentResult>,
}

/// One schedulable entity. The root loop and every sub-agent are uniform `Tcb`s.
#[derive(Debug, Clone)]
pub struct Tcb {
    pub id: TaskId,
    pub parent: Option<TaskId>,
    pub state: TaskLifecycle,
    pub budget: BudgetLedger,
    pub wait: Option<WaitReason>,
    /// Capability ids permitted to this task (mirrors `AgentProcess.permitted_capability_ids`).
    pub caps: Vec<CompactString>,
    /// Sub-agent identity for child tasks; `None` for the root loop.
    pub proc: Option<ProcInfo>,
}

impl Tcb {
    /// The root loop task. Constructed from the runtime task at `Start`.
    pub fn root(id: impl Into<TaskId>, budget: SchedulerBudget) -> Self {
        Self {
            id: id.into(),
            parent: None,
            state: TaskLifecycle::Ready,
            budget: BudgetLedger::new(budget),
            wait: None,
            caps: Vec::new(),
            proc: None,
        }
    }

    /// A sub-agent task spawned under the root, seeded `Running`, carrying the manifest's
    /// process identity. The single source of truth for what the `AgentProcess` view exposes.
    pub fn spawned(manifest: &IsolationManifest, budget: SchedulerBudget) -> Self {
        Self::spawned_in(manifest, budget, TaskLifecycle::Running)
    }

    /// The same child, seeded in an explicit lifecycle state.
    ///
    /// §10.4's spawn arc is `PendingLaunch → Starting → Running(ack)`; the legacy path collapses it
    /// to `Running` at insert time, which is why [`Self::spawned`] exists unchanged and this is the
    /// entry point the ack-gated canonical path uses.
    pub fn spawned_in(
        manifest: &IsolationManifest,
        budget: SchedulerBudget,
        state: TaskLifecycle,
    ) -> Self {
        Self {
            id: manifest.agent_id.clone(),
            parent: Some("root".into()),
            state,
            budget: BudgetLedger::new(budget),
            wait: None,
            caps: manifest.permitted_capability_ids.clone(),
            proc: Some(ProcInfo {
                role: manifest.role,
                isolation: manifest.isolation,
                context_inheritance: manifest.context_inheritance,
                result: None,
            }),
        }
    }
}

/// Unified registry of all tasks: the root loop plus one child per sub-agent. The sole source of
/// truth for schedulability and lineage; the `AgentProcess` view is derived from it.
#[derive(Debug, Clone, Default)]
pub struct TaskTable {
    tasks: Vec<Tcb>,
}

impl TaskTable {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, tcb: Tcb) {
        if let Some(existing) = self.tasks.iter_mut().find(|t| t.id == tcb.id) {
            *existing = tcb;
        } else {
            self.tasks.push(tcb);
        }
    }

    pub fn get(&self, id: &str) -> Option<&Tcb> {
        self.tasks.iter().find(|t| t.id.as_str() == id)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut Tcb> {
        self.tasks.iter_mut().find(|t| t.id.as_str() == id)
    }

    pub fn all(&self) -> &[Tcb] {
        &self.tasks
    }

    pub fn children_of(&self, parent: &str) -> Vec<&Tcb> {
        self.tasks
            .iter()
            .filter(|t| t.parent.as_deref() == Some(parent))
            .collect()
    }
}

/// Pure budget verdict for one task: `Some(reason)` when a budget axis (turn/token/wall)
/// is exhausted, mapped to the same `TerminationReason` the state machine applies.
/// The single budget decision point — evaluated at each turn boundary.
pub fn budget_verdict(task: &Tcb, now_ms: Option<u64>) -> Option<TerminationReason> {
    task.budget.exceeded(now_ms).map(|axis| match axis {
        "max_turns" => TerminationReason::MaxTurns,
        "wall_time" => TerminationReason::Timeout,
        _ => TerminationReason::TokenBudget,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_state_maps_to_lifecycle() {
        assert_eq!(
            TaskLifecycle::from(ProcessState::Running),
            TaskLifecycle::Running
        );
        assert_eq!(
            TaskLifecycle::from(ProcessState::Joined),
            TaskLifecycle::Done(TerminationReason::Completed)
        );
        assert_eq!(
            TaskLifecycle::from(ProcessState::Failed),
            TaskLifecycle::Done(TerminationReason::Error)
        );
    }

    #[test]
    fn budget_ledger_delegates_to_scheduler_budget() {
        let mut ledger = BudgetLedger::new(SchedulerBudget {
            max_turns: 2,
            ..SchedulerBudget::default()
        });
        assert_eq!(ledger.exceeded(None), None);
        ledger.turns = 2;
        assert_eq!(ledger.exceeded(None), Some("max_turns"));
    }

    #[test]
    fn task_table_insert_and_lineage() {
        let mut table = TaskTable::new();
        table.insert(Tcb::root("root", SchedulerBudget::default()));
        let mut child = Tcb::root("child", SchedulerBudget::default());
        child.parent = Some("root".into());
        table.insert(child);

        assert_eq!(table.children_of("root").len(), 1);
        assert!(table.get("root").is_some());
    }

    #[test]
    fn task_table_insert_is_idempotent_by_id() {
        let mut table = TaskTable::new();
        table.insert(Tcb::root("root", SchedulerBudget::default()));
        let mut updated = Tcb::root("root", SchedulerBudget::default());
        updated.state = TaskLifecycle::Running;
        table.insert(updated);

        assert_eq!(table.all().len(), 1);
        assert_eq!(table.get("root").unwrap().state, TaskLifecycle::Running);
    }

    #[test]
    fn budget_verdict_none_within_budget() {
        let tcb = Tcb::root(
            "root",
            SchedulerBudget {
                max_turns: 5,
                ..SchedulerBudget::default()
            },
        );
        assert_eq!(budget_verdict(&tcb, None), None);
    }

    #[test]
    fn budget_verdict_matches_should_terminate_axis() {
        let limits = SchedulerBudget {
            max_turns: 2,
            ..SchedulerBudget::default()
        };
        let mut tcb = Tcb::root("root", limits.clone());
        tcb.budget.turns = 2;
        // budget_verdict and the underlying budget check must agree on verdict and reason.
        assert_eq!(limits.should_terminate(2, 0, None, None), Some("max_turns"));
        assert_eq!(
            budget_verdict(&tcb, None),
            Some(TerminationReason::MaxTurns)
        );
    }

    #[test]
    fn budget_verdict_wall_time_maps_to_timeout() {
        let limits = SchedulerBudget {
            max_wall_ms: Some(1_000),
            ..SchedulerBudget::default()
        };
        let mut tcb = Tcb::root("root", limits);
        tcb.budget.started_at_ms = Some(0);
        assert_eq!(
            budget_verdict(&tcb, Some(2_000)),
            Some(TerminationReason::Timeout)
        );
    }

    #[test]
    fn baseline_token_budget_terminates() {
        let limits = SchedulerBudget {
            max_total_tokens: 100,
            ..SchedulerBudget::default()
        };
        let mut tcb = Tcb::root("root", limits);
        tcb.budget.total_tokens = 200;
        assert_eq!(
            budget_verdict(&tcb, None),
            Some(TerminationReason::TokenBudget)
        );
    }
}
