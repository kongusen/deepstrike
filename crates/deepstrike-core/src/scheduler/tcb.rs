//! Primitive P2: Task Control Block + unified scheduling entity.
//!
//! M0 scaffold (see `.local-docs/specs/agent-os-three-primitives.md`): types + conversions
//! only — **no wiring, no behavior change**. A later milestone (M1) folds the root loop and
//! every sub-agent into a single `Tcb` and replaces the scattered
//! `LoopPhase` lifecycle variants + `SchedulerBudget::should_terminate` + `ProcessTable`
//! with `TaskTable` + a pure `schedule()` function.
//!
//! Concept overlap this primitive collapses:
//! - lifecycle written twice ([`crate::scheduler::state_machine::LoopPhase`] lifecycle variants /
//!   [`SuspendReason`] / [`BlockReason`] vs [`crate::proc::ProcessState`]) → [`TaskState`];
//! - suspend/block reasons (two enums) → [`WaitReason`].

use compact_str::CompactString;
use serde::{Deserialize, Serialize};

use crate::proc::ProcessState;
use crate::scheduler::policy::SchedulerBudget;
use crate::scheduler::state_machine::{BlockReason, SuspendReason};
use crate::types::agent::{AgentIsolation, AgentRole, ContextInheritance, IsolationManifest};
use crate::types::result::{SubAgentResult, TerminationReason};

/// Identity of a schedulable task. Task 0 is the root loop; children are sub-agents.
/// Aligns with `AgentProcess.agent_id` so M1 can map process rows onto TCBs 1:1.
pub type TaskId = CompactString;

/// Schedulability of a task — orthogonal to the *intra-turn* step
/// (`Reason/Act/Observe/Delta`), which stays on [`crate::scheduler::state_machine::LoopPhase`].
///
/// Unifies `LoopPhase::{Idle,Suspended,Blocked,Terminal}` and
/// [`ProcessState::{Running,Joined,Failed}`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    /// Eligible to run, not yet picked by the scheduler (`LoopPhase::Idle`).
    Ready,
    /// Currently executing a turn (`LoopPhase::{Reason,Act,Observe,Delta}` / `ProcessState::Running`).
    Running,
    /// Blocked awaiting an in-flight continuation (tool suspend / milestone eval).
    Blocked,
    /// Suspended awaiting external resolution (human approval / sub-agent join / external).
    Suspended,
    /// Finished. Carries the termination reason (`ProcessState::{Joined,Failed}` + `LoopPhase::Terminal`).
    Done(TerminationReason),
}

impl TaskState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Running => "running",
            Self::Blocked => "blocked",
            Self::Suspended => "suspended",
            Self::Done(_) => "done",
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Done(_))
    }
}

/// A successful join maps to `Done(Completed)`; any other termination is `Done(<reason>)`.
impl From<ProcessState> for TaskState {
    fn from(state: ProcessState) -> Self {
        match state {
            ProcessState::Running => TaskState::Running,
            ProcessState::Joined => TaskState::Done(TerminationReason::Completed),
            // Failed has no single reason at the process level; M1 carries the real reason
            // from `SubAgentResult`. Scaffold maps to a generic error.
            ProcessState::Failed => TaskState::Done(TerminationReason::Error),
        }
    }
}

/// Why a task is not runnable. Unifies [`SuspendReason`] and [`BlockReason`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitReason {
    /// Governance `AskUser` — waiting for SDK to resolve human approval.
    Approval,
    /// Parent blocked on a child task's join result.
    SubAgentJoin(TaskId),
    /// Awaiting a tool continuation (tool suspend pattern).
    Tool,
    /// Awaiting milestone evaluation result.
    Milestone,
    /// Awaiting a routed signal at a turn boundary.
    Signal,
    /// Externally requested suspension.
    External,
}

impl WaitReason {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Approval => "approval",
            Self::SubAgentJoin(_) => "sub_agent_join",
            Self::Tool => "tool",
            Self::Milestone => "milestone",
            Self::Signal => "signal",
            Self::External => "external",
        }
    }
}

impl From<SuspendReason> for WaitReason {
    fn from(reason: SuspendReason) -> Self {
        match reason {
            SuspendReason::AskUser => WaitReason::Approval,
            // The child id is not known at this scaffold boundary; M1 supplies it.
            SuspendReason::SubAgentAwait => WaitReason::SubAgentJoin(TaskId::default()),
            SuspendReason::External => WaitReason::External,
        }
    }
}

impl From<BlockReason> for WaitReason {
    fn from(reason: BlockReason) -> Self {
        match reason {
            BlockReason::ToolSuspend => WaitReason::Tool,
            BlockReason::MilestoneAwait => WaitReason::Milestone,
        }
    }
}

/// Running budget counters + limits for a task. Wraps the existing [`SchedulerBudget`]
/// limits so M1 can move `should_terminate` evaluation here without changing the axes.
#[derive(Debug, Clone)]
pub struct BudgetLedger {
    pub limits: SchedulerBudget,
    pub turns: u32,
    pub total_tokens: u64,
    pub started_at_ms: Option<u64>,
}

impl BudgetLedger {
    pub fn new(limits: SchedulerBudget) -> Self {
        Self { limits, turns: 0, total_tokens: 0, started_at_ms: None }
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

/// The budget a task is granted for the next run step. M1's `schedule()` returns one of these.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BudgetSlice {
    pub max_turns: u32,
    pub max_total_tokens: u64,
    pub max_wall_ms: Option<u64>,
}

/// Sub-agent-specific identity carried by a child [`Tcb`]; `None` on the root task.
///
/// This is what lets [`crate::proc::ProcessTable`] become a *derived view* over the
/// [`TaskTable`]: every child task whose `proc` is `Some` reconstructs exactly one
/// [`crate::proc::AgentProcess`] (see [`crate::proc::AgentProcess::from_tcb`]). The previously
/// duplicated process storage collapses into these fields.
#[derive(Debug, Clone)]
pub struct ProcInfo {
    pub parent_session_id: CompactString,
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
    pub state: TaskState,
    pub budget: BudgetLedger,
    pub wait: Option<WaitReason>,
    /// Capability ids permitted to this task (mirrors `AgentProcess.permitted_capability_ids`).
    pub caps: Vec<CompactString>,
    /// Sub-agent identity for child tasks; `None` for the root loop.
    pub proc: Option<ProcInfo>,
}

impl Tcb {
    /// The root loop task (id 0). M1 constructs this from the runtime task at `Start`.
    pub fn root(id: impl Into<TaskId>, budget: SchedulerBudget) -> Self {
        Self {
            id: id.into(),
            parent: None,
            state: TaskState::Ready,
            budget: BudgetLedger::new(budget),
            wait: None,
            caps: Vec::new(),
            proc: None,
        }
    }

    /// A sub-agent task spawned under the root, seeded `Running`, carrying the manifest's
    /// process identity. The single source of truth for what was previously an `AgentProcess`
    /// row in a separate `ProcessTable`.
    pub fn spawned(manifest: &IsolationManifest, budget: SchedulerBudget) -> Self {
        Self {
            id: manifest.agent_id.clone(),
            parent: Some("root".into()),
            state: TaskState::Running,
            budget: BudgetLedger::new(budget),
            wait: None,
            caps: manifest.permitted_capability_ids.clone(),
            proc: Some(ProcInfo {
                parent_session_id: manifest.parent_session_id.clone(),
                role: manifest.role,
                isolation: manifest.isolation,
                context_inheritance: manifest.context_inheritance,
                result: None,
            }),
        }
    }

    pub fn is_runnable(&self) -> bool {
        matches!(self.state, TaskState::Ready)
    }
}

/// Unified registry of all tasks. Generalizes [`crate::proc::ProcessTable`]; M1 makes
/// the process table a view over this.
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

    pub fn runnable(&self) -> Vec<&Tcb> {
        self.tasks.iter().filter(|t| t.is_runnable()).collect()
    }
}

/// Result of a pure scheduling pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScheduleDecision {
    Run { task: TaskId, slice: BudgetSlice },
    Suspend { task: TaskId, reason: WaitReason },
    Terminate { task: TaskId, reason: TerminationReason },
}

/// Pure scheduling decision for a single task's budget axes.
///
/// M1b spine: encodes the **same verdict** as [`SchedulerBudget::should_terminate`], expressed
/// over a [`Tcb`]. It is wired into the state machine in parallel with the legacy path under a
/// `debug_assert` (zero behavior change) so the equivalence is proven before it becomes the single
/// decision point. Later milestones extend this to pick among multiple runnable tasks + apply
/// signal preemption — at which point the legacy `should_terminate` call site is removed.
pub fn schedule(task: &Tcb, now_ms: Option<u64>) -> ScheduleDecision {
    if let Some(reason) = task.budget.exceeded(now_ms) {
        // Same axis-name → TerminationReason mapping the state machine applies today.
        let term = match reason {
            "max_turns" => TerminationReason::MaxTurns,
            "wall_time" => TerminationReason::Timeout,
            _ => TerminationReason::TokenBudget,
        };
        return ScheduleDecision::Terminate { task: task.id.clone(), reason: term };
    }
    ScheduleDecision::Run {
        task: task.id.clone(),
        slice: BudgetSlice {
            max_turns: task.budget.limits.max_turns,
            max_total_tokens: task.budget.limits.max_total_tokens,
            max_wall_ms: task.budget.limits.max_wall_ms,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_state_maps_to_task_state() {
        assert_eq!(TaskState::from(ProcessState::Running), TaskState::Running);
        assert_eq!(
            TaskState::from(ProcessState::Joined),
            TaskState::Done(TerminationReason::Completed)
        );
        assert_eq!(
            TaskState::from(ProcessState::Failed),
            TaskState::Done(TerminationReason::Error)
        );
    }

    #[test]
    fn suspend_reason_maps_to_wait_reason() {
        assert_eq!(WaitReason::from(SuspendReason::AskUser), WaitReason::Approval);
        assert_eq!(WaitReason::from(SuspendReason::External), WaitReason::External);
        assert!(matches!(
            WaitReason::from(SuspendReason::SubAgentAwait),
            WaitReason::SubAgentJoin(_)
        ));
    }

    #[test]
    fn block_reason_maps_to_wait_reason() {
        assert_eq!(WaitReason::from(BlockReason::ToolSuspend), WaitReason::Tool);
        assert_eq!(
            WaitReason::from(BlockReason::MilestoneAwait),
            WaitReason::Milestone
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
        assert!(table.get("root").unwrap().is_runnable());
        assert_eq!(table.runnable().len(), 2);
    }

    #[test]
    fn schedule_runs_when_within_budget() {
        let tcb = Tcb::root("root", SchedulerBudget { max_turns: 5, ..SchedulerBudget::default() });
        assert!(matches!(schedule(&tcb, None), ScheduleDecision::Run { .. }));
    }

    #[test]
    fn schedule_terminates_and_matches_should_terminate_axis() {
        let limits = SchedulerBudget { max_turns: 2, ..SchedulerBudget::default() };
        let mut tcb = Tcb::root("root", limits.clone());
        tcb.budget.turns = 2;
        // schedule() and the legacy budget check must agree on both verdict and reason.
        let legacy = limits.should_terminate(2, 0, None, None);
        assert_eq!(legacy, Some("max_turns"));
        match schedule(&tcb, None) {
            ScheduleDecision::Terminate { reason, .. } => {
                assert_eq!(reason, TerminationReason::MaxTurns)
            }
            other => panic!("expected Terminate, got {other:?}"),
        }
    }

    #[test]
    fn schedule_terminates_on_wall_time_as_timeout() {
        let limits = SchedulerBudget { max_wall_ms: Some(1_000), ..SchedulerBudget::default() };
        let mut tcb = Tcb::root("root", limits);
        tcb.budget.started_at_ms = Some(0);
        match schedule(&tcb, Some(2_000)) {
            ScheduleDecision::Terminate { reason, .. } => {
                assert_eq!(reason, TerminationReason::Timeout)
            }
            other => panic!("expected Terminate, got {other:?}"),
        }
    }

    #[test]
    fn task_table_insert_is_idempotent_by_id() {
        let mut table = TaskTable::new();
        table.insert(Tcb::root("root", SchedulerBudget::default()));
        let mut updated = Tcb::root("root", SchedulerBudget::default());
        updated.state = TaskState::Running;
        table.insert(updated);
        assert_eq!(table.all().len(), 1);
        assert_eq!(table.get("root").unwrap().state, TaskState::Running);
    }
}
