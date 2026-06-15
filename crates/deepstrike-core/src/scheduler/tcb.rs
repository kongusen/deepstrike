//! Primitive P2: Task Control Block + unified scheduling entity.
//!
//! See `.local-docs/specs/agent-os-three-primitives.md`. M1 收口 wired this in: the root loop and
//! every sub-agent are a single `Tcb`, and the scattered `LoopPhase` lifecycle variants +
//! `SchedulerBudget::should_terminate` + the former `ProcessTable` collapsed into the `TaskTable`
//! plus the pure `schedule()` function (`schedule()` is now the sole budget decision point;
//! `AgentProcess` is a derived view over child TCBs).
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
    /// Parent blocked on child tasks' join results. Tracks pending child IDs.
    /// W2-1: Changed from single TaskId to Vec to support workflow batches.
    SubAgentJoin(Vec<TaskId>),
    /// Awaiting a tool continuation (tool suspend pattern).
    Tool,
    /// Awaiting milestone evaluation result.
    Milestone,
    /// Awaiting a routed signal at a turn boundary.
    ///
    /// **Descoped (v0.2.11):** Signal→Schedule integration was explicitly descoped.
    /// The variant is tested infrastructure (~6 tests) and deserialized by `snapshot.rs`,
    /// but is not wired into any production code path. Retained for snapshot compatibility
    /// and future reactivation.
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

    /// W2-1: Remove a completed child from the SubAgentJoin list.
    /// Returns true if this was the last pending child (task should become runnable).
    pub fn remove_child(&mut self, child_id: &str) -> bool {
        if let Self::SubAgentJoin(children) = self {
            children.retain(|id| id.as_str() != child_id);
            children.is_empty()
        } else {
            false
        }
    }

    /// W2-1: Check if a specific child is in the pending list.
    pub fn has_child(&self, child_id: &str) -> bool {
        if let Self::SubAgentJoin(children) = self {
            children.iter().any(|id| id.as_str() == child_id)
        } else {
            false
        }
    }
}

impl From<SuspendReason> for WaitReason {
    fn from(reason: SuspendReason) -> Self {
        match reason {
            SuspendReason::AskUser => WaitReason::Approval,
            // The child id is not known at this scaffold boundary; M1 supplies it.
            // W2-1: Changed to empty vec (will be populated with actual child IDs at spawn).
            SuspendReason::SubAgentAwait => WaitReason::SubAgentJoin(Vec::new()),
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
/// This is what makes the `AgentProcess` view *derived* from the [`TaskTable`]: every child task
/// whose `proc` is `Some` reconstructs exactly one [`crate::proc::AgentProcess`] (see
/// [`crate::proc::AgentProcess::from_tcb`]). The formerly duplicated process storage collapses
/// into these fields.
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
    /// W2-1: Tasks hitting quota/deferred conditions get a deferred timestamp.
    /// When set, the task is considered Ready-but-deferred until `now_ms >= deferred_until`.
    pub deferred_until: Option<u64>,
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
            deferred_until: None,
        }
    }

    /// A sub-agent task spawned under the root, seeded `Running`, carrying the manifest's
    /// process identity. The single source of truth for what the `AgentProcess` view exposes.
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
            deferred_until: None,
        }
    }

    /// Whether this task is eligible to run now (Ready state + not deferred).
    pub fn is_runnable(&self) -> bool {
        self.is_runnable_at(None)
    }

    /// Whether this task is eligible to run at a given timestamp.
    pub fn is_runnable_at(&self, now_ms: Option<u64>) -> bool {
        if !matches!(self.state, TaskState::Ready) {
            return false;
        }
        match self.deferred_until {
            Some(deferred) => match now_ms {
                Some(now) => now >= deferred,
                None => false, // Without time, deferred tasks are not runnable
            },
            None => true,
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

    pub fn runnable(&self) -> Vec<&Tcb> {
        self.runnable_at(None)
    }

    /// Runnable tasks at a given timestamp (accounts for deferred tasks).
    pub fn runnable_at(&self, now_ms: Option<u64>) -> Vec<&Tcb> {
        self.tasks.iter().filter(|t| t.is_runnable_at(now_ms)).collect()
    }
}

/// Result of a pure scheduling pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScheduleDecision {
    /// No tasks are runnable — scheduler is idle.
    Idle,
    /// Run the specified task with the given budget slice.
    Run { task: TaskId, slice: BudgetSlice },
    /// Suspend the specified task (e.g., awaiting external resolution).
    Suspend { task: TaskId, reason: WaitReason },
    /// Terminate the specified task.
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

/// W2-1: Multi-task scheduler — picks one task to run from the TaskTable.
///
/// This is the "true scheduler" that:
/// 1. Checks budget on all tasks and terminates any that exceeded
/// 2. Filters runnable tasks (Ready + not deferred)
/// 3. Applies signal-aware prioritization (TODO: W2-1 full signal integration)
/// 4. Returns `Idle` if no runnable tasks, or `Run` for the selected task
///
/// For now, prioritization is simple FIFO (first runnable task wins).
/// Future W2-1 work will integrate signal urgency and parent-child priority.
///
/// `highest_signal_urgency`: Optional urgency level (0-3) of the highest priority
/// pending signal. When set, tasks waiting on Signal with matching or higher
/// urgency are prioritized.
///
/// **Descoped (v0.2.11):** The `highest_signal_urgency` parameter is tested
/// infrastructure only — no production caller supplies a `Some` value today.
/// Signal→Schedule integration was explicitly descoped; the parameter is retained
/// so the prioritization logic is exercised by unit tests and ready for future
/// reactivation without an API change.
pub fn schedule_multi(table: &TaskTable, now_ms: Option<u64>, highest_signal_urgency: Option<u8>) -> ScheduleDecision {
    // First pass: check all tasks for budget termination
    for task in table.all() {
        if let Some(reason) = task.budget.exceeded(now_ms) {
            let term = match reason {
                "max_turns" => TerminationReason::MaxTurns,
                "wall_time" => TerminationReason::Timeout,
                _ => TerminationReason::TokenBudget,
            };
            return ScheduleDecision::Terminate { task: task.id.clone(), reason: term };
        }
    }

    // Second pass: filter runnable tasks
    let runnable = table.runnable_at(now_ms);

    if runnable.is_empty() {
        return ScheduleDecision::Idle;
    }

    // W2-1: Signal-aware prioritization
    // If there's a high priority signal, prefer tasks that might be responsive to it
    let selected = if let Some(urgency) = highest_signal_urgency {
        // High urgency (Critical=3, High=2): prefer tasks with Signal wait reason
        if urgency >= 2 {
            runnable
                .iter()
                .find(|t| matches!(t.wait, Some(WaitReason::Signal)))
                .unwrap_or_else(|| runnable.first().expect("runnable non-empty"))
        } else {
            runnable.first().expect("runnable non-empty")
        }
    } else {
        runnable.first().expect("runnable non-empty")
    };

    ScheduleDecision::Run {
        task: selected.id.clone(),
        slice: BudgetSlice {
            max_turns: selected.budget.limits.max_turns,
            max_total_tokens: selected.budget.limits.max_total_tokens,
            max_wall_ms: selected.budget.limits.max_wall_ms,
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

    // W2-1: multi-task scheduler tests

    #[test]
    fn schedule_multi_returns_idle_when_no_runnable() {
        let table = TaskTable::new();
        match schedule_multi(&table, None, None) {
            ScheduleDecision::Idle => {}
            other => panic!("expected Idle, got {:?}", other),
        }
    }

    #[test]
    fn schedule_multi_runs_single_ready_task() {
        let mut table = TaskTable::new();
        table.insert(Tcb::root("root", SchedulerBudget { max_turns: 5, ..SchedulerBudget::default() }));
        match schedule_multi(&table, None, None) {
            ScheduleDecision::Run { task, .. } => {
                assert_eq!(task.as_str(), "root");
            }
            other => panic!("expected Run, got {:?}", other),
        }
    }

    #[test]
    fn schedule_multi_terminates_over_budget_tasks() {
        let mut table = TaskTable::new();
        let limits = SchedulerBudget { max_turns: 2, ..SchedulerBudget::default() };
        let mut root = Tcb::root("root", limits);
        root.budget.turns = 2; // over budget
        table.insert(root);
        match schedule_multi(&table, None, None) {
            ScheduleDecision::Terminate { reason, .. } => {
                assert_eq!(reason, TerminationReason::MaxTurns);
            }
            other => panic!("expected Terminate, got {:?}", other),
        }
    }

    #[test]
    fn schedule_multi_skips_deferred_tasks() {
        let mut table = TaskTable::new();
        let mut root = Tcb::root("root", SchedulerBudget::default());
        root.deferred_until = Some(999_999); // deferred far into future
        table.insert(root);

        // With no timestamp, deferred tasks are not runnable
        assert_eq!(table.runnable_at(None).len(), 0);

        // With timestamp in the past, task becomes runnable
        assert_eq!(table.runnable_at(Some(1_000_000)).len(), 1);
    }

    #[test]
    fn schedule_multi_picks_first_runnable_fifo() {
        let mut table = TaskTable::new();
        table.insert(Tcb::root("task-a", SchedulerBudget::default()));
        table.insert(Tcb::root("task-b", SchedulerBudget::default()));
        table.insert(Tcb::root("task-c", SchedulerBudget::default()));

        // Simple FIFO: first task in list wins
        match schedule_multi(&table, None, None) {
            ScheduleDecision::Run { task, .. } => {
                assert_eq!(task.as_str(), "task-a");
            }
            other => panic!("expected Run, got {:?}", other),
        }
    }

    #[test]
    fn schedule_multi_ignores_blocked_tasks() {
        let mut table = TaskTable::new();
        let mut blocked = Tcb::root("blocked", SchedulerBudget::default());
        blocked.state = TaskState::Blocked;
        table.insert(blocked);
        table.insert(Tcb::root("ready", SchedulerBudget::default()));

        match schedule_multi(&table, None, None) {
            ScheduleDecision::Run { task, .. } => {
                assert_eq!(task.as_str(), "ready");
            }
            other => panic!("expected Run, got {:?}", other),
        }
    }

    #[test]
    fn schedule_multi_ignores_suspended_tasks() {
        let mut table = TaskTable::new();
        let mut suspended = Tcb::root("suspended", SchedulerBudget::default());
        suspended.state = TaskState::Suspended;
        table.insert(suspended);
        table.insert(Tcb::root("ready", SchedulerBudget::default()));

        match schedule_multi(&table, None, None) {
            ScheduleDecision::Run { task, .. } => {
                assert_eq!(task.as_str(), "ready");
            }
            other => panic!("expected Run, got {:?}", other),
        }
    }

    #[test]
    fn schedule_multi_ignores_done_tasks() {
        let mut table = TaskTable::new();
        let mut done = Tcb::root("done", SchedulerBudget::default());
        done.state = TaskState::Done(TerminationReason::Completed);
        table.insert(done);
        table.insert(Tcb::root("ready", SchedulerBudget::default()));

        match schedule_multi(&table, None, None) {
            ScheduleDecision::Run { task, .. } => {
                assert_eq!(task.as_str(), "ready");
            }
            other => panic!("expected Run, got {:?}", other),
        }
    }

    #[test]
    fn deferred_task_becomes_runnable_after_time() {
        let mut tcb = Tcb::root("root", SchedulerBudget::default());
        tcb.deferred_until = Some(1000);

        // Before defer time: not runnable
        assert!(!tcb.is_runnable_at(Some(999)));

        // At defer time: runnable
        assert!(tcb.is_runnable_at(Some(1000)));

        // After defer time: runnable
        assert!(tcb.is_runnable_at(Some(1001)));
    }

    /// W2-1: Demonstrate how deferred tasks are skipped during scheduling.
    /// This is the mechanism for quota backpressure: tasks that hit quota limits
    /// get a `deferred_until` timestamp and are not scheduled until that time passes.
    #[test]
    fn schedule_multi_skips_deferred_and_returns_next_ready() {
        let mut table = TaskTable::new();

        // Task A is deferred
        let mut task_a = Tcb::root("task-a", SchedulerBudget::default());
        task_a.deferred_until = Some(999_999);
        table.insert(task_a);

        // Task B is ready
        table.insert(Tcb::root("task-b", SchedulerBudget::default()));

        // Task C is also ready
        table.insert(Tcb::root("task-c", SchedulerBudget::default()));

        // With no time context, deferred tasks are not runnable
        match schedule_multi(&table, None, None) {
            ScheduleDecision::Run { task, .. } => {
                assert_eq!(task.as_str(), "task-b");
            }
            other => panic!("expected Run, got {:?}", other),
        }

        // With time past defer threshold, task-a becomes runnable
        match schedule_multi(&table, Some(1_000_000), None) {
            ScheduleDecision::Run { task, .. } => {
                assert_eq!(task.as_str(), "task-a");
            }
            other => panic!("expected Run, got {:?}", other),
        }
    }

    // W2-1: Signal-aware prioritization tests

    #[test]
    fn signal_aware_prioritization_with_no_signal() {
        let mut table = TaskTable::new();
        table.insert(Tcb::root("task-a", SchedulerBudget::default()));
        table.insert(Tcb::root("task-b", SchedulerBudget::default()));

        // No signal urgency: FIFO selection
        match schedule_multi(&table, None, None) {
            ScheduleDecision::Run { task, .. } => {
                assert_eq!(task.as_str(), "task-a");
            }
            other => panic!("expected Run, got {:?}", other),
        }
    }

    #[test]
    fn signal_aware_prioritization_prefers_signal_waiting_task() {
        let mut table = TaskTable::new();
        table.insert(Tcb::root("normal-task", SchedulerBudget::default()));

        let mut waiting = Tcb::root("signal-waiting", SchedulerBudget::default());
        waiting.wait = Some(WaitReason::Signal);
        table.insert(waiting);

        // High urgency signal: prefer the task waiting on Signal
        match schedule_multi(&table, None, Some(3)) {
            ScheduleDecision::Run { task, .. } => {
                assert_eq!(task.as_str(), "signal-waiting");
            }
            other => panic!("expected Run signal-waiting, got {:?}", other),
        }
    }

    #[test]
    fn signal_aware_prioritization_normal_signal_no_prefer() {
        let mut table = TaskTable::new();
        table.insert(Tcb::root("task-a", SchedulerBudget::default()));

        let mut waiting = Tcb::root("signal-waiting", SchedulerBudget::default());
        waiting.wait = Some(WaitReason::Signal);
        table.insert(waiting);

        // Normal urgency signal: FIFO (no preference)
        match schedule_multi(&table, None, Some(1)) {
            ScheduleDecision::Run { task, .. } => {
                assert_eq!(task.as_str(), "task-a");
            }
            other => panic!("expected Run task-a, got {:?}", other),
        }
    }

    #[test]
    fn signal_aware_prioritization_high_signal_prefer() {
        let mut table = TaskTable::new();
        table.insert(Tcb::root("task-a", SchedulerBudget::default()));

        let mut waiting = Tcb::root("signal-waiting", SchedulerBudget::default());
        waiting.wait = Some(WaitReason::Signal);
        table.insert(waiting);

        // High urgency signal (2): prefer signal-waiting
        match schedule_multi(&table, None, Some(2)) {
            ScheduleDecision::Run { task, .. } => {
                assert_eq!(task.as_str(), "signal-waiting");
            }
            other => panic!("expected Run signal-waiting, got {:?}", other),
        }
    }

    #[test]
    fn signal_aware_prioritization_critical_signal_strongly_prefer() {
        let mut table = TaskTable::new();
        table.insert(Tcb::root("first", SchedulerBudget::default()));
        table.insert(Tcb::root("second", SchedulerBudget::default()));

        let mut waiting = Tcb::root("critical-waiting", SchedulerBudget::default());
        waiting.wait = Some(WaitReason::Signal);
        table.insert(waiting);

        // Critical urgency (3): strongly prefer signal-waiting
        match schedule_multi(&table, None, Some(3)) {
            ScheduleDecision::Run { task, .. } => {
                assert_eq!(task.as_str(), "critical-waiting");
            }
            other => panic!("expected Run critical-waiting, got {:?}", other),
        }
    }

    // W2-1: Golden baseline tests for multi-task scheduler

    #[test]
    fn baseline_single_task_selection() {
        let mut table = TaskTable::new();
        let task = Tcb::root("root", SchedulerBudget {
            max_tokens: 1000,
            max_turns: 10,
            max_total_tokens: 5000,
            max_wall_ms: None,
        });
        table.insert(task);

        let decision = schedule_multi(&table, None, None);

        // Golden baseline: single task should be selected with its budget limits
        match decision {
            ScheduleDecision::Run { task: id, slice } => {
                assert_eq!(id.as_str(), "root");
                assert_eq!(slice.max_turns, 10);
                assert_eq!(slice.max_total_tokens, 5000);
            }
            other => panic!("Expected Run, got {:?}", other),
        }
    }

    #[test]
    fn baseline_fifo_selection_order() {
        let mut table = TaskTable::new();
        table.insert(Tcb::root("task-1", SchedulerBudget::default()));
        table.insert(Tcb::root("task-2", SchedulerBudget::default()));
        table.insert(Tcb::root("task-3", SchedulerBudget::default()));

        // Golden baseline: tasks should be selected in FIFO order (insertion order)
        let decision1 = schedule_multi(&table, None, None);
        match decision1 {
            ScheduleDecision::Run { task, .. } => assert_eq!(task.as_str(), "task-1"),
            _ => panic!("Expected Run task-1"),
        }

        // After removing task-1, task-2 should be selected
        table.tasks.remove(0);
        let decision2 = schedule_multi(&table, None, None);
        match decision2 {
            ScheduleDecision::Run { task, .. } => assert_eq!(task.as_str(), "task-2"),
            _ => panic!("Expected Run task-2"),
        }
    }

    #[test]
    fn baseline_idle_when_no_runnable() {
        let table = TaskTable::new();

        let decision = schedule_multi(&table, None, None);

        // Golden baseline: no tasks means Idle
        assert!(matches!(decision, ScheduleDecision::Idle));
    }

    #[test]
    fn baseline_terminates_over_budget() {
        let mut table = TaskTable::new();
        let mut task = Tcb::root("over-budget", SchedulerBudget {
            max_turns: 5,
            max_total_tokens: 1000,
            max_wall_ms: None,
            max_tokens: 1000,
        });
        task.budget.turns = 10; // Exceeded max_turns
        table.insert(task);

        let decision = schedule_multi(&table, None, None);

        // Golden baseline: over-budget tasks should be terminated
        match decision {
            ScheduleDecision::Terminate { reason, .. } => {
                assert_eq!(reason, TerminationReason::MaxTurns);
            }
            other => panic!("Expected Terminate, got {:?}", other),
        }
    }

    #[test]
    fn baseline_token_budget_terminates() {
        let mut table = TaskTable::new();
        let mut task = Tcb::root("token-over", SchedulerBudget {
            max_turns: 100,
            max_total_tokens: 100,
            max_wall_ms: None,
            max_tokens: 1000,
        });
        task.budget.total_tokens = 200; // Exceeded max_total_tokens
        table.insert(task);

        let decision = schedule_multi(&table, None, None);

        // Golden baseline: token budget exceeded should terminate
        match decision {
            ScheduleDecision::Terminate { reason, .. } => {
                assert_eq!(reason, TerminationReason::TokenBudget);
            }
            other => panic!("Expected Terminate, got {:?}", other),
        }
    }

    #[test]
    fn baseline_wall_time_timeout() {
        let mut table = TaskTable::new();
        let mut task = Tcb::root("timeout", SchedulerBudget {
            max_turns: 100,
            max_total_tokens: 10000,
            max_wall_ms: Some(1000),
            max_tokens: 1000,
        });
        task.budget.started_at_ms = Some(0);
        table.insert(task);

        let decision = schedule_multi(&table, Some(2000), None);

        // Golden baseline: wall time exceeded should terminate with Timeout
        match decision {
            ScheduleDecision::Terminate { reason, .. } => {
                assert_eq!(reason, TerminationReason::Timeout);
            }
            other => panic!("Expected Terminate, got {:?}", other),
        }
    }

    #[test]
    fn monotonicity_termination_first_before_selection() {
        let mut table = TaskTable::new();

        // Add an over-budget task
        let mut over_budget = Tcb::root("over-budget", SchedulerBudget {
            max_turns: 5,
            max_total_tokens: 1000,
            max_wall_ms: None,
            max_tokens: 1000,
        });
        over_budget.budget.turns = 10;
        table.insert(over_budget);

        // Add a healthy task
        table.insert(Tcb::root("healthy", SchedulerBudget::default()));

        // Monotonicity: termination should always be checked before selection
        let decision = schedule_multi(&table, None, None);

        // Should terminate the over-budget task, not run the healthy one
        match decision {
            ScheduleDecision::Terminate { task, .. } => {
                assert_eq!(task.as_str(), "over-budget");
            }
            other => panic!("Expected Terminate over-budget, got {:?}", other),
        }
    }

    #[test]
    fn monotonicity_deferred_not_selected_before_time() {
        let mut table = TaskTable::new();
        table.insert(Tcb::root("ready", SchedulerBudget::default()));

        let mut deferred = Tcb::root("deferred", SchedulerBudget::default());
        deferred.deferred_until = Some(999_999);
        table.insert(deferred);

        // Before defer time: deferred task should not be selected
        let decision = schedule_multi(&table, Some(0), None);

        match decision {
            ScheduleDecision::Run { task, .. } => {
                assert_eq!(task.as_str(), "ready");
            }
            other => panic!("Expected Run ready, got {:?}", other),
        }
    }

    #[test]
    fn monotonicity_blocked_suspended_not_selected() {
        let mut table = TaskTable::new();
        table.insert(Tcb::root("ready", SchedulerBudget::default()));

        let mut blocked = Tcb::root("blocked", SchedulerBudget::default());
        blocked.state = TaskState::Blocked;
        blocked.wait = Some(WaitReason::Tool);
        table.insert(blocked);

        let mut suspended = Tcb::root("suspended", SchedulerBudget::default());
        suspended.state = TaskState::Suspended;
        suspended.wait = Some(WaitReason::Approval);
        table.insert(suspended);

        // Only ready tasks should be selected
        let decision = schedule_multi(&table, None, None);

        match decision {
            ScheduleDecision::Run { task, .. } => {
                assert_eq!(task.as_str(), "ready");
            }
            other => panic!("Expected Run ready, got {:?}", other),
        }
    }

    #[test]
    fn baseline_signal_aware_selection() {
        let mut table = TaskTable::new();
        table.insert(Tcb::root("normal", SchedulerBudget::default()));

        let mut signal_waiting = Tcb::root("signal-task", SchedulerBudget::default());
        signal_waiting.wait = Some(WaitReason::Signal);
        table.insert(signal_waiting);

        // With critical signal: signal-waiting task should be preferred
        let decision = schedule_multi(&table, None, Some(3));

        match decision {
            ScheduleDecision::Run { task, .. } => {
                assert_eq!(task.as_str(), "signal-task");
            }
            other => panic!("Expected Run signal-task, got {:?}", other),
        }
    }
}
