//! Primitive P2: Task Control Block + unified scheduling entity.
//!
//! See `.local-docs/specs/agent-os-three-primitives.md`. The root loop and every
//! sub-agent are a single [`Tcb`]; the [`TaskTable`] is the sole source of truth for
//! schedulability and lineage, and the `AgentProcess` view is derived from it.
//! [`budget_verdict`] is the single budget decision point (turn/token/wall axes),
//! delegating to [`SchedulerBudget::should_terminate`] via [`BudgetLedger`].

use std::collections::{BTreeMap, BTreeSet};

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
    /// Every spawn constructs this state; publication advances it to [`Self::Starting`] and only a
    /// correlated launch acknowledgement advances it to [`Self::Running`].
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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunnableCause {
    #[default]
    NestedTask,
    TimerWaiter,
    MessageWaiter,
    EventWaiter,
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

/// spc_003 §2: placeholder identity newtypes for wait conditions that have no existing kernel
/// concept yet. Minimal on purpose — no validation, no dependency beyond `CompactString` — until a
/// real producer (spc_003-05+ for `Timer`; future cards for the rest) earns a richer type.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ApprovalId(pub CompactString);

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SignalFilter(pub CompactString);

/// Milliseconds, matching the existing wall-clock convention (`now_ms`/`max_wall_ms`/
/// `started_at_ms` are all `u64` ms elsewhere in this module).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct LogicalDeadline(pub u64);

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ChannelId(pub CompactString);

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ResourceKey(pub CompactString);

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SubscriptionId(pub CompactString);

/// spc_003 §2: the generalized wait vocabulary `WaitReason` is expected to grow into. Additive
/// only in this card — not wired to `Tcb.wait` yet (spc_003-04).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WaitCondition {
    Effect(crate::runtime::kernel::wire::EffectId),
    Child(TaskId),
    Children(Vec<TaskId>),
    Approval(ApprovalId),
    Signal(SignalFilter),
    Timer(LogicalDeadline),
    Channel(ChannelId),
    Resource(ResourceKey),
    External(SubscriptionId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WaitMode {
    Any,
    All,
}

/// spc_003 §2. Additive only in this card — not wired to `Tcb.wait` yet (spc_003-04).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaitSet {
    pub mode: WaitMode,
    pub conditions: Vec<WaitCondition>,
}

/// The durable half of a task wait. `WaitIndex` can always be rebuilt from `wait_set`; partial
/// `All` progress cannot, so the satisfied condition indexes live on the TCB and in checkpoints.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DurableWaitSet {
    pub mode: WaitMode,
    pub conditions: Vec<WaitCondition>,
    pub satisfied: BTreeSet<usize>,
}

impl From<WaitSet> for DurableWaitSet {
    fn from(wait_set: WaitSet) -> Self {
        Self {
            mode: wait_set.mode,
            conditions: wait_set.conditions,
            satisfied: BTreeSet::new(),
        }
    }
}

/// spc_003-02: semantic-equivalence mapping for the two existing `WaitReason` variants. `Approval`
/// carries no id anywhere in the kernel today (verified by grep — the AskUser path constructs a
/// bare `WaitReason::Approval`), so this mints a fixed placeholder id until governance's AskUser
/// path earns a real one.
impl From<WaitReason> for (WaitCondition, WaitMode) {
    fn from(reason: WaitReason) -> Self {
        match reason {
            WaitReason::Approval => (
                WaitCondition::Approval(ApprovalId(CompactString::from("pending"))),
                WaitMode::Any,
            ),
            WaitReason::SubAgentJoin(ids) => (WaitCondition::Children(ids), WaitMode::All),
        }
    }
}

fn durable_wait_from_reason(reason: &WaitReason) -> WaitSet {
    match reason {
        WaitReason::Approval => WaitSet {
            mode: WaitMode::Any,
            conditions: vec![WaitCondition::Approval(ApprovalId(CompactString::from(
                "pending",
            )))],
        },
        WaitReason::SubAgentJoin(ids) => WaitSet {
            mode: WaitMode::All,
            conditions: ids.iter().cloned().map(WaitCondition::Child).collect(),
        },
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

/// spc_002 §4: what happens to a task's children when the task itself terminates. Additive type
/// only in this card — not wired to parent-termination logic yet (a future card, once a real
/// producer needs it).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChildFailurePolicy {
    #[default]
    Propagate,
    Isolate,
    Restart,
    Retry,
    Ignore,
}

/// spc_002 §4.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SupervisionPolicy {
    pub child_failure: ChildFailurePolicy,
    pub max_restarts: Option<u32>,
    pub cancel_children_on_exit: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupervisionEvent {
    pub attempt: u32,
    pub strategy: ChildFailurePolicy,
    pub reason: CompactString,
    /// Every event closes one attempt; `relaunched` says whether the logical task continued.
    pub terminal: bool,
    pub relaunched: bool,
}

impl Default for SupervisionPolicy {
    fn default() -> Self {
        Self {
            child_failure: ChildFailurePolicy::default(),
            max_restarts: None,
            cancel_children_on_exit: true,
        }
    }
}

/// One schedulable entity. The root loop and every sub-agent are uniform `Tcb`s.
#[derive(Debug, Clone)]
pub struct Tcb {
    pub id: TaskId,
    pub parent: Option<TaskId>,
    /// spc_002: child task ids spawned under this task. Populated by `TaskTable` insertion
    /// (spc_002-05); empty for leaf tasks and for every task predating recursive spawn.
    pub children: BTreeSet<TaskId>,
    pub state: TaskLifecycle,
    /// Why a Ready child entered the unified local runnable set.
    pub runnable_cause: RunnableCause,
    pub budget: BudgetLedger,
    pub wait: Option<WaitReason>,
    /// Canonical recoverable wait state. `wait` remains the compatibility projection for the two
    /// legacy reasons; new heterogeneous waits use this field as their source of truth.
    pub wait_set: Option<DurableWaitSet>,
    /// Capability ids permitted to this task (mirrors `AgentProcess.permitted_capability_ids`).
    pub caps: Vec<CompactString>,
    /// spc_004 debt closure: this task's own held `Capability` grants (spc_004's resource/
    /// action-scoped shape) — what a spawn from *this* task may legally attenuate into. Empty by
    /// default; only set when `IsolationManifest.requested_capabilities` (spc_004) was non-empty
    /// at spawn time.
    pub capabilities: Vec<crate::types::capability::Capability>,
    /// Sub-agent identity for child tasks; `None` for the root loop.
    pub proc: Option<ProcInfo>,
    /// spc_002-07: read by `terminate()` (spc_008-04) when this task's own terminal transition
    /// commits, to decide what happens to any still-running children.
    pub supervision: SupervisionPolicy,
    /// Durable attempt-level failure history. Logical-task state may continue after a relaunch.
    pub supervision_events: Vec<SupervisionEvent>,
    /// spc_008-05: when `true`, this task is exempt from `cancel_subtree`/`cancel_children` —
    /// it (and its own descendants) are never cancelled as a side effect of an ancestor's
    /// cancellation or termination. `false` by default (existing behavior unchanged).
    pub detached: bool,
    /// spc_005: this task's own currently-grantable budget pool for its children — `reserve()`'s
    /// `parent_remaining` argument. `None` (the default) means no hierarchical budget is in play
    /// on this task, so spawn-time budget checks are skipped entirely and behavior is unchanged
    /// from before spc_005.
    pub child_budget_remaining: Option<super::budget_grant::ResourceBudget>,
    /// spc_005: the grant this task itself received from its parent at spawn time, if the
    /// spawner set `IsolationManifest.requested_budget`. `None` for the root and for any child
    /// spawned without a hierarchical budget request.
    pub budget_grant: Option<super::budget_grant::BudgetGrant>,
    /// spc_006-03: this task's point-to-point inbox. Empty by default; populated via
    /// `TaskTable::send_message`.
    pub mailbox: super::mailbox::Mailbox,
}

impl Tcb {
    /// The root loop task. Constructed from the runtime task at `Start`.
    pub fn root(id: impl Into<TaskId>, budget: SchedulerBudget) -> Self {
        Self {
            id: id.into(),
            parent: None,
            children: BTreeSet::new(),
            state: TaskLifecycle::Ready,
            runnable_cause: RunnableCause::NestedTask,
            budget: BudgetLedger::new(budget),
            wait: None,
            wait_set: None,
            caps: Vec::new(),
            capabilities: Vec::new(),
            proc: None,
            supervision: SupervisionPolicy::default(),
            supervision_events: Vec::new(),
            detached: false,
            child_budget_remaining: None,
            budget_grant: None,
            mailbox: super::mailbox::Mailbox::new(),
        }
    }

    /// A sub-agent task spawned under the root, seeded `Running`, carrying the manifest's
    /// process identity. The single source of truth for what the `AgentProcess` view exposes.
    pub fn spawned(manifest: &IsolationManifest, budget: SchedulerBudget) -> Self {
        Self::spawned_in(
            manifest,
            budget,
            TaskLifecycle::Running,
            Some(TaskId::from("root")),
        )
    }

    /// The same child, seeded in an explicit lifecycle state under an explicit `parent`.
    ///
    /// §10.4's spawn arc is `PendingLaunch → Starting → Running(ack)`; the legacy path collapses it
    /// to `Running` at insert time, which is why [`Self::spawned`] exists unchanged and this is the
    /// entry point the ack-gated canonical path uses.
    ///
    /// spc_002-02: `parent` is no longer hardcoded to `root` — callers must supply the real caller.
    /// This card does not derive that caller from syscall causation (spc_002-04); every call site
    /// still passes `Some("root")` explicitly, preserving today's behavior byte-for-byte.
    pub fn spawned_in(
        manifest: &IsolationManifest,
        budget: SchedulerBudget,
        state: TaskLifecycle,
        parent: Option<TaskId>,
    ) -> Self {
        Self {
            id: manifest.agent_id.clone(),
            parent,
            children: BTreeSet::new(),
            state,
            runnable_cause: RunnableCause::NestedTask,
            budget: BudgetLedger::new(budget),
            wait: None,
            wait_set: None,
            caps: manifest.permitted_capability_ids.clone(),
            capabilities: manifest.requested_capabilities.clone(),
            proc: Some(ProcInfo {
                role: manifest.role,
                isolation: manifest.isolation,
                context_inheritance: manifest.context_inheritance,
                result: None,
            }),
            supervision: SupervisionPolicy::default(),
            supervision_events: Vec::new(),
            detached: false,
            child_budget_remaining: None,
            budget_grant: None,
            mailbox: super::mailbox::Mailbox::new(),
        }
    }
}

/// Unified registry of all tasks: the root loop plus one child per sub-agent. The sole source of
/// truth for schedulability and lineage; the `AgentProcess` view is derived from it.
#[derive(Debug, Clone, Default)]
pub struct TaskTable {
    tasks: Vec<Tcb>,
    /// spc_003-04: kept in sync with every task's `wait` field exclusively through
    /// [`Self::set_wait`] — that method is the only sanctioned way to mutate `Tcb.wait`.
    wait_index: super::wait_index::WaitIndex,
    channels: BTreeMap<ChannelId, super::mailbox::Channel>,
    objects: BTreeMap<crate::mm::handle::ObjectId, crate::mm::handle::ObjectDescriptor>,
}

/// Rejections from the kernel-owned child creation entrypoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TaskSpawnError {
    UnknownCaller,
    CallerTerminal,
    DuplicateTask,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LocalIpcError {
    UnknownCaller,
    CallerTerminal,
    UnknownRecipient,
    ChannelSubscribersMismatch,
    NotSubscriber,
    Full,
    Expired,
    ObjectConflict,
}

impl TaskTable {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, tcb: Tcb) {
        // spc_002-05: a fresh child registers itself in its parent's `children` set. Re-inserting
        // an existing id (state update, not a new spawn) must not touch any `children` set.
        let is_new = !self.tasks.iter().any(|t| t.id == tcb.id);
        if is_new
            && let Some(parent_id) = tcb.parent.clone()
            && let Some(parent) = self.tasks.iter_mut().find(|t| t.id == parent_id)
        {
            parent.children.insert(tcb.id.clone());
        }
        if let Some(existing) = self.tasks.iter_mut().find(|t| t.id == tcb.id) {
            *existing = tcb;
        } else {
            self.tasks.push(tcb);
        }
    }

    /// Create a child under a caller already present in this table.
    ///
    /// This is the only sanctioned construction path for runtime children. The caller supplies
    /// the parent identity, while the manifest cannot smuggle one in; canonical driver code is
    /// responsible for deriving that caller from kernel-owned causation before calling here.
    pub(crate) fn spawn_child(
        &mut self,
        caller: &str,
        manifest: &IsolationManifest,
        budget: SchedulerBudget,
        state: TaskLifecycle,
    ) -> Result<TaskId, TaskSpawnError> {
        let parent = self.get(caller).ok_or(TaskSpawnError::UnknownCaller)?;
        if parent.state.is_terminal() {
            return Err(TaskSpawnError::CallerTerminal);
        }
        if self.get(manifest.agent_id.as_str()).is_some() {
            return Err(TaskSpawnError::DuplicateTask);
        }

        let child_id = manifest.agent_id.clone();
        let child = Tcb::spawned_in(manifest, budget, state, Some(caller.into()));
        self.insert(child);
        Ok(child_id)
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

    pub(crate) fn runnable_candidates(&self) -> Vec<super::runnable::LocalRunnable> {
        let mut tasks: Vec<_> = self
            .tasks
            .iter()
            .filter(|task| task.proc.is_some() && task.state == TaskLifecycle::Ready)
            .collect();
        tasks.sort_by(|left, right| left.id.cmp(&right.id));
        tasks
            .into_iter()
            .map(|task| super::runnable::LocalRunnable {
                id: task.id.to_string(),
                kind: match task.runnable_cause {
                    RunnableCause::NestedTask => super::runnable::LocalRunnableKind::NestedTask,
                    RunnableCause::TimerWaiter => super::runnable::LocalRunnableKind::TimerWaiter,
                    RunnableCause::MessageWaiter => {
                        super::runnable::LocalRunnableKind::MessageWaiter
                    }
                    RunnableCause::EventWaiter => super::runnable::LocalRunnableKind::EventWaiter,
                },
                source_rank: 0,
            })
            .collect()
    }

    pub fn children_of(&self, parent: &str) -> Vec<&Tcb> {
        self.tasks
            .iter()
            .filter(|t| t.parent.as_deref() == Some(parent))
            .collect()
    }

    /// Number of parent edges from the structural root to `task_id`. Returns `None` for an
    /// unknown task or malformed/cyclic lineage; spawn itself cannot create such a cycle.
    pub(crate) fn lineage_depth(&self, task_id: &str) -> Option<usize> {
        let mut current = self.get(task_id)?;
        let mut depth = 0usize;
        while let Some(parent) = current.parent.as_ref() {
            depth = depth.checked_add(1)?;
            if depth > self.tasks.len() {
                return None;
            }
            current = self.get(parent.as_str())?;
        }
        Some(depth)
    }

    pub fn wait_index(&self) -> &super::wait_index::WaitIndex {
        &self.wait_index
    }

    /// spc_003-04: the only sanctioned path for mutating `Tcb.wait` — keeps `WaitIndex` in sync
    /// with the field it mirrors. `None` clears any existing wait; `Some` replaces it (the prior
    /// wait, if any, is removed from the index first). A `task_id` not present in the table is a
    /// no-op.
    pub fn set_wait(&mut self, task_id: &str, wait: Option<WaitReason>) {
        let Some(id) = self.get(task_id).map(|t| t.id.clone()) else {
            return;
        };
        if let Some(old) = self.get_mut(task_id).and_then(|t| t.wait.take()) {
            let (condition, _mode) = <(WaitCondition, WaitMode)>::from(old);
            self.wait_index.remove(&id, &condition);
        }
        self.clear_durable_wait(&id);
        if let Some(task) = self.get_mut(task_id) {
            task.wait = wait.clone();
        }
        if let Some(new_wait) = wait {
            let wait_set = durable_wait_from_reason(&new_wait);
            if !wait_set.conditions.is_empty() {
                self.wait_index
                    .register_wait_set(id.clone(), wait_set.clone());
                if let Some(task) = self.get_mut(task_id) {
                    task.wait_set = Some(wait_set.into());
                }
            }
        }
    }

    /// spc_003-05: register `task_id` as waiting on a deadline. `Timer` has no `WaitReason`
    /// counterpart (spc_003-01 does not touch `WaitReason`), so — unlike `set_wait` — this only
    /// updates the `WaitIndex`, not `Tcb.wait`.
    pub fn wait_for_timer(&mut self, task_id: &str, deadline: LogicalDeadline) {
        self.register_wait_set(
            task_id,
            WaitSet {
                mode: WaitMode::Any,
                conditions: vec![WaitCondition::Timer(deadline)],
            },
        );
    }

    /// spc_003-05: wake every task whose `Timer` deadline is `<= now_ms`. Returns the woken ids.
    pub fn wake_expired_timers(&mut self, now_ms: u64) -> Vec<TaskId> {
        self.wait_index
            .due_timer_keys(now_ms)
            .into_iter()
            .flat_map(|key| self.notify(&key))
            .collect()
    }

    /// spc_003-06: register `task_id` as waiting on an arbitrary `WaitCondition`, index-only
    /// (mirrors [`Self::wait_for_timer`] — no `Tcb.wait` mutation, since most `WaitCondition`
    /// variants have no `WaitReason` counterpart to store there).
    pub fn wait_for_condition(&mut self, task_id: &str, condition: &WaitCondition) {
        self.register_wait_set(
            task_id,
            WaitSet {
                mode: WaitMode::Any,
                conditions: vec![condition.clone()],
            },
        );
    }

    /// spc_003-06: wake every task waiting on exactly `key`. Idempotent — see
    /// [`super::wait_index::WaitIndex::wake`].
    pub fn wake(&mut self, key: &super::wait_index::WaitKey) -> Vec<TaskId> {
        self.notify(key)
    }

    /// spc_003 debt closure: register `task_id` against a full `WaitSet` (`Any`/`All` over
    /// multiple heterogeneous conditions) — see [`super::wait_index::WaitIndex::register_wait_set`].
    pub fn register_wait_set(&mut self, task_id: &str, wait_set: WaitSet) {
        let Some(id) = self.get(task_id).map(|t| t.id.clone()) else {
            return;
        };
        if wait_set.conditions.is_empty()
            || self
                .get(task_id)
                .is_some_and(|task| task.state.is_terminal())
        {
            return;
        }
        self.clear_durable_wait(&id);
        self.wait_index
            .register_wait_set(id.clone(), wait_set.clone());
        if let Some(task) = self.get_mut(task_id) {
            task.state = TaskLifecycle::Suspended;
            task.wait = None;
            task.wait_set = Some(wait_set.into());
        }
    }

    /// spc_003 debt closure: notify every task registered under `key` via
    /// [`Self::register_wait_set`], returning those whose whole `WaitSet` is now satisfied — see
    /// [`super::wait_index::WaitIndex::notify`].
    pub fn notify(&mut self, key: &super::wait_index::WaitKey) -> Vec<TaskId> {
        let mut candidates = self.wait_index.lookup(key).to_vec();
        // The reverse index is reconstructed from checkpoint task rows, so its bucket insertion
        // order can differ from the live registration order. Scheduling order must depend only on
        // durable identity, not on that ephemeral history.
        candidates.sort_unstable();
        let mut woken = Vec::new();
        for task_id in candidates {
            let terminal = self
                .get(task_id.as_str())
                .is_none_or(|task| task.state.is_terminal());
            if terminal {
                self.clear_durable_wait(&task_id);
                if let Some(task) = self.get_mut(task_id.as_str()) {
                    task.wait = None;
                }
                continue;
            }

            let satisfied = if let Some(task) = self.get_mut(task_id.as_str())
                && let Some(wait_set) = task.wait_set.as_mut()
            {
                for (index, condition) in wait_set.conditions.iter().enumerate() {
                    if key.matches(condition) {
                        wait_set.satisfied.insert(index);
                    }
                }
                match wait_set.mode {
                    WaitMode::Any => !wait_set.satisfied.is_empty(),
                    WaitMode::All => wait_set.satisfied.len() == wait_set.conditions.len(),
                }
            } else {
                false
            };

            if satisfied {
                let cause = match key {
                    super::wait_index::WaitKey::Timer(_) => RunnableCause::TimerWaiter,
                    super::wait_index::WaitKey::Channel(_)
                    | super::wait_index::WaitKey::External(_) => RunnableCause::MessageWaiter,
                    _ => RunnableCause::EventWaiter,
                };
                self.clear_durable_wait(&task_id);
                if let Some(task) = self.get_mut(task_id.as_str())
                    && task.state == TaskLifecycle::Suspended
                {
                    task.state = TaskLifecycle::Ready;
                    task.runnable_cause = cause;
                    task.wait = None;
                    woken.push(task_id);
                }
            }
        }
        woken
    }

    fn clear_durable_wait(&mut self, task_id: &TaskId) {
        let wait_set = self
            .get_mut(task_id.as_str())
            .and_then(|task| task.wait_set.take());
        if let Some(wait_set) = wait_set {
            for condition in &wait_set.conditions {
                self.wait_index.remove(task_id, condition);
            }
        }
    }

    /// spc_002-04: the id of this table's own structural root — the task with no `parent` — the
    /// kernel-owned fact a spawn's `parent` derives from instead of a hardcoded `"root"` literal.
    /// `None` only for a table that has not yet had its root task inserted.
    pub fn root_id(&self) -> Option<TaskId> {
        self.tasks
            .iter()
            .find(|t| t.parent.is_none())
            .map(|t| t.id.clone())
    }

    /// spc_002-06: recursively cancel `task_id` and every (recursive) child. Default cancellation
    /// policy — no detached-child exemption yet (future card, per spc_002 §5).
    pub fn cancel_subtree(&mut self, task_id: &str) {
        let children: Vec<TaskId> = self
            .get(task_id)
            .map(|t| t.children.iter().cloned().collect())
            .unwrap_or_default();
        if let Some(task) = self.get_mut(task_id) {
            task.state = TaskLifecycle::Done(TerminationReason::UserAbort);
        }
        for child_id in children {
            // spc_008-05: a detached child (and, transitively, everything under it — this `if`
            // skips recursing into it at all, so its own children are never reached either) is
            // exempt from cancellation cascading down from an ancestor.
            let is_detached = self
                .get(child_id.as_str())
                .is_some_and(|child| child.detached);
            if !is_detached {
                self.cancel_subtree(child_id.as_str());
            }
        }
    }

    /// spc_008-04: like [`Self::cancel_subtree`] but leaves `task_id` itself untouched — only its
    /// (recursive) descendants are cancelled. For `SupervisionPolicy.child_failure ==
    /// ChildFailurePolicy::Propagate`, where the terminating task's own termination reason (set by
    /// the caller) must not be overwritten by this call. Already-terminal children are skipped so a
    /// child that already completed successfully is never retroactively relabeled as cancelled;
    /// spc_008-05: a `detached` direct child is skipped too, same exemption `cancel_subtree`'s own
    /// recursion honors — `Propagate` must not reach into a child that opted out of the lifecycle.
    pub fn cancel_children(&mut self, task_id: &str) {
        let children: Vec<TaskId> = self
            .get(task_id)
            .map(|t| t.children.iter().cloned().collect())
            .unwrap_or_default();
        for child_id in children {
            if self
                .get(child_id.as_str())
                .is_some_and(|t| !t.state.is_terminal() && !t.detached)
            {
                self.cancel_subtree(child_id.as_str());
            }
        }
    }

    pub(crate) fn prepare_supervised_relaunch(
        &mut self,
        task_id: &str,
        strategy: ChildFailurePolicy,
    ) {
        self.set_wait(task_id, None);
        if let Some(task) = self.get_mut(task_id) {
            task.state = TaskLifecycle::PendingLaunch;
            if let Some(proc) = task.proc.as_mut() {
                proc.result = None;
            }
            if strategy == ChildFailurePolicy::Restart {
                task.budget.turns = 0;
                task.budget.total_tokens = 0;
            }
        }
    }

    /// spc_005-05: `return_unused` a child's `budget_grant` (if it has one) into its parent's
    /// `child_budget_remaining`, then record `returned` on the grant for audit. A no-op when the
    /// child never carried a grant (spawned without `requested_budget`, or the parent had no
    /// `child_budget_remaining` set — see spc_005-04) or when the parent has since lost its own
    /// remaining-budget pool. Idempotent to call twice: the second call finds `consumed` already
    /// reflecting the first return_unused's inputs and returns the same (already-zeroed) delta —
    /// callers still should call this exactly once per terminal transition.
    pub fn return_child_budget(&mut self, child_id: &str) {
        let Some(grant) = self.get(child_id).and_then(|t| t.budget_grant.clone()) else {
            return;
        };
        if grant.settled {
            return;
        }
        let unused = super::budget_grant::return_unused(&grant);
        if let Some(child) = self.get_mut(child_id)
            && let Some(child_grant) = child.budget_grant.as_mut()
        {
            child_grant.returned = unused;
            child_grant.settled = true;
        }
        if let Some(parent) = self.get_mut(grant.parent.as_str()) {
            if let Some(remaining) = parent.child_budget_remaining {
                parent.child_budget_remaining =
                    Some(super::budget_grant::credit(&remaining, &unused));
            }
            // A descendant's actual usage is part of this parent's own received grant. Roll it
            // upward once so settling the parent cannot refund resources a grandchild consumed.
            if let Some(parent_grant) = parent.budget_grant.as_mut()
                && !parent_grant.settled
            {
                parent_grant.consumed =
                    super::budget_grant::accumulate_usage(&parent_grant.consumed, &grant.consumed);
            }
        }
    }

    /// Attach a just-reserved hierarchical grant to its child and seed that child's own grantable
    /// pool from the reservation. This is the atomic bridge from spawn gate accounting to the TCB.
    pub(crate) fn attach_child_budget_grant(
        &mut self,
        child_id: &str,
        grant: super::budget_grant::BudgetGrant,
    ) {
        if grant.child.as_str() != child_id {
            return;
        }
        if let Some(child) = self.get_mut(child_id) {
            child.child_budget_remaining = Some(grant.reserved);
            child.budget_grant = Some(grant);
        }
    }

    /// spc_006-03: deliver `msg` into `msg.to`'s mailbox. A no-op (message silently dropped) when
    /// `to` names no task in this table — mirrors `get_mut`'s own "absent id, no panic" contract
    /// used throughout this table's other mutators.
    pub fn send_message(&mut self, msg: super::mailbox::MailboxMessage) {
        if let Some(recipient) = self.get_mut(msg.to.as_str()) {
            recipient.mailbox.send(msg);
        }
    }

    /// Canonical point-to-point delivery. `from` is overwritten from kernel-derived causation;
    /// duplicate ids are successful no-ops, while capacity/TTL failures are explicit.
    pub(crate) fn send_message_from(
        &mut self,
        caller: &str,
        mut msg: super::mailbox::MailboxMessage,
        now: super::mailbox::LogicalTime,
    ) -> Result<bool, LocalIpcError> {
        let caller_task = self.get(caller).ok_or(LocalIpcError::UnknownCaller)?;
        if caller_task.state.is_terminal() {
            return Err(LocalIpcError::CallerTerminal);
        }
        if self.get(msg.to.as_str()).is_none() {
            return Err(LocalIpcError::UnknownRecipient);
        }
        msg.from = caller.into();
        let recipient_id = msg.to.clone();
        let outcome = self
            .get_mut(recipient_id.as_str())
            .expect("recipient was validated")
            .mailbox
            .try_send(msg, now);
        match outcome {
            super::mailbox::IpcEnqueueOutcome::Accepted => {
                self.notify(&super::wait_index::WaitKey::External(SubscriptionId(
                    format!("mailbox:{recipient_id}").into(),
                )));
                Ok(true)
            }
            super::mailbox::IpcEnqueueOutcome::Duplicate => Ok(false),
            super::mailbox::IpcEnqueueOutcome::Full => Err(LocalIpcError::Full),
            super::mailbox::IpcEnqueueOutcome::Expired => Err(LocalIpcError::Expired),
        }
    }

    pub(crate) fn receive_mailbox(
        &mut self,
        caller: &str,
        now: super::mailbox::LogicalTime,
        max: usize,
    ) -> Result<Vec<super::mailbox::MailboxMessage>, LocalIpcError> {
        let task = self.get_mut(caller).ok_or(LocalIpcError::UnknownCaller)?;
        let mut messages = Vec::new();
        for _ in 0..max {
            let Some(message) = task.mailbox.receive_at(now) else {
                break;
            };
            messages.push(message);
        }
        Ok(messages)
    }

    pub(crate) fn publish_channel(
        &mut self,
        caller: &str,
        channel_id: ChannelId,
        subscribers: Vec<TaskId>,
        mut msg: super::mailbox::MailboxMessage,
        now: super::mailbox::LogicalTime,
    ) -> Result<bool, LocalIpcError> {
        let caller_task = self.get(caller).ok_or(LocalIpcError::UnknownCaller)?;
        if caller_task.state.is_terminal() {
            return Err(LocalIpcError::CallerTerminal);
        }
        if subscribers.iter().any(|id| self.get(id.as_str()).is_none()) {
            return Err(LocalIpcError::UnknownRecipient);
        }
        msg.from = caller.into();
        let channel = self
            .channels
            .entry(channel_id.clone())
            .or_insert_with(|| super::mailbox::Channel::new(subscribers.clone()));
        if channel.subscribers != subscribers {
            return Err(LocalIpcError::ChannelSubscribersMismatch);
        }
        match channel.publish_at(msg, now) {
            super::mailbox::IpcEnqueueOutcome::Accepted => {
                self.notify(&super::wait_index::WaitKey::Channel(channel_id));
                Ok(true)
            }
            super::mailbox::IpcEnqueueOutcome::Duplicate => Ok(false),
            super::mailbox::IpcEnqueueOutcome::Full => Err(LocalIpcError::Full),
            super::mailbox::IpcEnqueueOutcome::Expired => Err(LocalIpcError::Expired),
        }
    }

    pub(crate) fn receive_channel(
        &mut self,
        caller: &str,
        channel_id: &ChannelId,
        now: super::mailbox::LogicalTime,
    ) -> Result<Vec<super::mailbox::MailboxMessage>, LocalIpcError> {
        if self.get(caller).is_none() {
            return Err(LocalIpcError::UnknownCaller);
        }
        let channel = self
            .channels
            .get_mut(channel_id)
            .ok_or(LocalIpcError::NotSubscriber)?;
        if !channel.subscribers.iter().any(|id| id.as_str() == caller) {
            return Err(LocalIpcError::NotSubscriber);
        }
        Ok(channel.drain_for_at(caller.into(), now))
    }

    pub(crate) fn channels(&self) -> &BTreeMap<ChannelId, super::mailbox::Channel> {
        &self.channels
    }

    pub(crate) fn restore_channels(
        &mut self,
        channels: BTreeMap<ChannelId, super::mailbox::Channel>,
    ) {
        self.channels = channels;
    }

    pub(crate) fn register_object(
        &mut self,
        caller: &str,
        descriptor: crate::mm::handle::ObjectDescriptor,
    ) -> Result<bool, LocalIpcError> {
        let caller_task = self.get(caller).ok_or(LocalIpcError::UnknownCaller)?;
        if caller_task.state.is_terminal() {
            return Err(LocalIpcError::CallerTerminal);
        }
        if descriptor.owner.as_str() != caller {
            return Err(LocalIpcError::UnknownCaller);
        }
        if let Some(existing) = self.objects.get(&descriptor.id) {
            return if existing == &descriptor {
                Ok(false)
            } else {
                Err(LocalIpcError::ObjectConflict)
            };
        }
        let resource = ResourceKey(format!("object:{}/{}", descriptor.owner, descriptor.id).into());
        self.objects.insert(descriptor.id, descriptor);
        self.notify(&super::wait_index::WaitKey::Resource(resource));
        Ok(true)
    }

    pub(crate) fn object(
        &self,
        object_id: crate::mm::handle::ObjectId,
    ) -> Option<&crate::mm::handle::ObjectDescriptor> {
        self.objects.get(&object_id)
    }

    pub(crate) fn objects(
        &self,
    ) -> &BTreeMap<crate::mm::handle::ObjectId, crate::mm::handle::ObjectDescriptor> {
        &self.objects
    }

    pub(crate) fn restore_objects(
        &mut self,
        objects: BTreeMap<crate::mm::handle::ObjectId, crate::mm::handle::ObjectDescriptor>,
    ) {
        self.objects = objects;
    }

    /// spc_002-09: recompute every task's `children` set from the authoritative `parent` field.
    /// `insert` only registers a child in its parent's `children` when the parent is already
    /// present in the table (spc_002-05) — a bulk restore that inserts rows in an order other
    /// than parent-before-child silently drops those edges. This is idempotent and safe to call
    /// after any bulk load.
    pub fn rebuild_children(&mut self) {
        for task in &mut self.tasks {
            task.children.clear();
        }
        let edges: Vec<(TaskId, TaskId)> = self
            .tasks
            .iter()
            .filter_map(|t| t.parent.clone().map(|parent| (parent, t.id.clone())))
            .collect();
        for (parent_id, child_id) in edges {
            if let Some(parent) = self.tasks.iter_mut().find(|t| t.id == parent_id) {
                parent.children.insert(child_id);
            }
        }
    }

    /// spc_002-09: reinsert every task's already-restored `Tcb.wait` into `WaitIndex`. A checkpoint
    /// restore sets `Tcb.wait` directly (it does not call [`Self::set_wait`], the index's only
    /// other sanctioned mutation path — restore reconstructs the whole table row at once, not one
    /// field at a time), so without this the index silently forgets every task that was waiting at
    /// checkpoint time. `insert` is idempotent per task id, so calling this more than once is safe.
    /// Durable WaitSets are reinserted from their unsatisfied conditions; the reverse index owns
    /// no lifecycle/progress state of its own.
    pub fn rebuild_wait_index(&mut self) {
        self.wait_index = super::wait_index::WaitIndex::new();
        let waiting: Vec<(TaskId, WaitReason)> = self
            .tasks
            .iter()
            .filter_map(|t| t.wait.clone().map(|wait| (t.id.clone(), wait)))
            .collect();
        for (id, wait) in waiting {
            let (condition, _mode) = <(WaitCondition, WaitMode)>::from(wait);
            self.wait_index.insert(id, &condition);
        }
        let durable: Vec<(TaskId, WaitSet)> = self
            .tasks
            .iter()
            .filter_map(|task| {
                task.wait_set.as_ref().map(|wait_set| {
                    (
                        task.id.clone(),
                        WaitSet {
                            mode: wait_set.mode,
                            conditions: wait_set
                                .conditions
                                .iter()
                                .enumerate()
                                .filter(|(index, _)| !wait_set.satisfied.contains(index))
                                .map(|(_, condition)| condition.clone())
                                .collect(),
                        },
                    )
                })
            })
            .collect();
        for (id, wait_set) in durable {
            self.wait_index.register_wait_set(id, wait_set);
        }
    }

    /// spc_002-08: process-tree invariant — no cycle in the `parent` chain of any task. Not on the
    /// spawn hot path (spawn already makes a cycle structurally unreachable, see spc_002-04); this
    /// exists to be tested and to back future debug/diagnostic tooling.
    pub fn has_cycle(&self) -> bool {
        for task in &self.tasks {
            let mut visited: BTreeSet<TaskId> = BTreeSet::new();
            let mut current = Some(task.id.clone());
            while let Some(id) = current {
                if !visited.insert(id.clone()) {
                    return true;
                }
                current = self.get(id.as_str()).and_then(|t| t.parent.clone());
            }
        }
        false
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
    use crate::types::agent::{AgentIdentity, AgentRole, AgentRunSpec};
    use crate::types::capability::CapabilityManifest;

    fn manifest_for(id: &str) -> IsolationManifest {
        let spec = AgentRunSpec::new(
            AgentIdentity::sub_agent(id, format!("{id}-session")),
            AgentRole::Implement,
            "do work",
        );
        IsolationManifest::from_spec(&spec, &CapabilityManifest::new())
    }

    #[test]
    fn spawned_in_uses_explicit_parent() {
        let manifest = manifest_for("agent-7-child");
        let tcb = Tcb::spawned_in(
            &manifest,
            SchedulerBudget::default(),
            TaskLifecycle::Running,
            Some(TaskId::from("agent-7")),
        );
        assert_eq!(tcb.parent, Some(TaskId::from("agent-7")));
    }

    #[test]
    fn spawn_child_derives_recursive_lineage_and_registers_each_edge_once() {
        let mut table = TaskTable::new();
        table.insert(Tcb::root("root", SchedulerBudget::default()));

        let child_manifest = manifest_for("child");
        let child_id = table
            .spawn_child(
                "root",
                &child_manifest,
                SchedulerBudget::default(),
                TaskLifecycle::Running,
            )
            .expect("root can spawn a child");

        let grandchild_manifest = manifest_for("grandchild");
        let grandchild_id = table
            .spawn_child(
                child_id.as_str(),
                &grandchild_manifest,
                SchedulerBudget::default(),
                TaskLifecycle::Running,
            )
            .expect("a live child can spawn its own child");

        assert_eq!(
            table
                .get(child_id.as_str())
                .and_then(|task| task.parent.clone()),
            Some(TaskId::from("root"))
        );
        assert_eq!(
            table
                .get(grandchild_id.as_str())
                .and_then(|task| task.parent.clone()),
            Some(child_id.clone())
        );
        assert_eq!(
            table
                .children_of("root")
                .iter()
                .map(|task| task.id.clone())
                .collect::<Vec<_>>(),
            vec![child_id.clone()]
        );
        assert_eq!(
            table
                .children_of(child_id.as_str())
                .iter()
                .map(|task| task.id.clone())
                .collect::<Vec<_>>(),
            vec![grandchild_id]
        );
        assert!(!table.has_cycle());
    }

    #[test]
    fn spawn_child_rejects_unknown_terminal_and_duplicate_callers() {
        let mut table = TaskTable::new();
        table.insert(Tcb::root("root", SchedulerBudget::default()));
        let manifest = manifest_for("child");

        assert_eq!(
            table.spawn_child(
                "missing",
                &manifest,
                SchedulerBudget::default(),
                TaskLifecycle::Running,
            ),
            Err(TaskSpawnError::UnknownCaller)
        );

        table
            .spawn_child(
                "root",
                &manifest,
                SchedulerBudget::default(),
                TaskLifecycle::Running,
            )
            .expect("first child creation succeeds");
        assert_eq!(
            table.spawn_child(
                "root",
                &manifest,
                SchedulerBudget::default(),
                TaskLifecycle::Running,
            ),
            Err(TaskSpawnError::DuplicateTask)
        );

        table.get_mut("root").unwrap().state = TaskLifecycle::Done(TerminationReason::Completed);
        let terminal_manifest = manifest_for("terminal-child");
        assert_eq!(
            table.spawn_child(
                "root",
                &terminal_manifest,
                SchedulerBudget::default(),
                TaskLifecycle::Running,
            ),
            Err(TaskSpawnError::CallerTerminal)
        );
    }

    #[test]
    fn spawned_in_carries_requested_capabilities_as_the_grant() {
        use crate::types::capability::{
            ActionSet, Capability, CapabilityId, ConstraintSet, Principal, ResourceSelector,
        };

        let mut manifest = manifest_for("child");
        let grant = Capability {
            id: CapabilityId("cap-1".into()),
            kind: crate::types::capability::CapabilityKind::Tool,
            resource: ResourceSelector("/repo/src/**".into()),
            actions: ActionSet(["read".into()].into_iter().collect()),
            constraints: ConstraintSet::default(),
            lease: None,
            delegatable: true,
            issuer: Principal("root".into()),
        };
        manifest.requested_capabilities = vec![grant.clone()];

        let tcb = Tcb::spawned_in(
            &manifest,
            SchedulerBudget::default(),
            TaskLifecycle::Running,
            Some(TaskId::from("root")),
        );
        assert_eq!(tcb.capabilities, vec![grant]);
    }

    #[test]
    fn root_task_has_no_capabilities_by_default() {
        let tcb = Tcb::root("root", SchedulerBudget::default());
        assert!(tcb.capabilities.is_empty());
    }

    #[test]
    fn nested_spawn_records_real_parent() {
        // Task A's own table: spawning B derives parent from the table's structural root (A),
        // not a hardcoded "root" literal.
        let mut table_a = TaskTable::new();
        table_a.insert(Tcb::root("A", SchedulerBudget::default()));
        let manifest_b = manifest_for("B");
        let b = Tcb::spawned_in(
            &manifest_b,
            SchedulerBudget::default(),
            TaskLifecycle::Running,
            table_a.root_id(),
        );
        assert_eq!(b.parent, Some(TaskId::from("A")));

        // B's own table (mirrors B running as its own kernel operation, rooted at its real id):
        // spawning C derives parent from B, not from A and not from a literal "root".
        let mut table_b = TaskTable::new();
        table_b.insert(Tcb::root(b.id.clone(), SchedulerBudget::default()));
        let manifest_c = manifest_for("C");
        let c = Tcb::spawned_in(
            &manifest_c,
            SchedulerBudget::default(),
            TaskLifecycle::Running,
            table_b.root_id(),
        );
        assert_eq!(c.parent, Some(TaskId::from("B")));
    }

    #[test]
    fn waiting_task_not_runnable() {
        let mut table = TaskTable::new();
        table.insert(Tcb::root("root", SchedulerBudget::default()));
        table.set_wait("root", Some(WaitReason::Approval));
        table.get_mut("root").unwrap().state = TaskLifecycle::Suspended;

        assert!(
            !table.get("root").unwrap().state.occupies_slot(),
            "a Suspended/Waiting task must not occupy a concurrency slot"
        );
        assert_ne!(table.get("root").unwrap().state, TaskLifecycle::Ready);
    }

    #[test]
    fn wake_is_idempotent() {
        let mut table = TaskTable::new();
        table.insert(Tcb::root("root", SchedulerBudget::default()));
        let effect = crate::runtime::kernel::wire::EffectId::new("e1").unwrap();
        table.wait_for_condition("root", &WaitCondition::Effect(effect.clone()));
        let key = crate::scheduler::wait_index::WaitKey::Effect(effect);

        let first = table.wake(&key);
        assert_eq!(first, vec![TaskId::from("root")]);

        // Simulated redelivery of the same completion event.
        let second = table.wake(&key);
        assert!(
            second.is_empty(),
            "a second wake for the same key must be a no-op"
        );
    }

    #[test]
    fn unrelated_event_does_not_wake() {
        let mut table = TaskTable::new();
        table.insert(Tcb::root("root", SchedulerBudget::default()));
        let e1 = crate::runtime::kernel::wire::EffectId::new("e1").unwrap();
        let e2 = crate::runtime::kernel::wire::EffectId::new("e2").unwrap();
        table.wait_for_condition("root", &WaitCondition::Effect(e1.clone()));

        let woken = table.wake(&crate::scheduler::wait_index::WaitKey::Effect(e2));
        assert!(woken.is_empty());
        assert_eq!(
            table
                .wait_index()
                .lookup(&crate::scheduler::wait_index::WaitKey::Effect(e1)),
            &[TaskId::from("root")],
            "task must still be registered as waiting on e1"
        );
    }

    #[test]
    fn task_table_wait_set_any_mode_wakes_through_the_public_api() {
        let mut table = TaskTable::new();
        table.insert(Tcb::root("root", SchedulerBudget::default()));
        let e1 = crate::runtime::kernel::wire::EffectId::new("e1").unwrap();
        let e2 = crate::runtime::kernel::wire::EffectId::new("e2").unwrap();
        table.register_wait_set(
            "root",
            WaitSet {
                mode: WaitMode::Any,
                conditions: vec![WaitCondition::Effect(e1.clone()), WaitCondition::Effect(e2)],
            },
        );

        let woken = table.notify(&crate::scheduler::wait_index::WaitKey::Effect(e1));
        assert_eq!(woken, vec![TaskId::from("root")]);
    }

    #[test]
    fn durable_wait_set_tracks_all_progress_on_the_tcb_and_wakes_ready_once() {
        use crate::scheduler::wait_index::WaitKey;

        let mut table = TaskTable::new();
        table.insert(Tcb::root("root", SchedulerBudget::default()));
        let first = crate::runtime::kernel::wire::EffectId::new("effect-1").unwrap();
        let second = crate::runtime::kernel::wire::EffectId::new("effect-2").unwrap();
        table.register_wait_set(
            "root",
            WaitSet {
                mode: WaitMode::All,
                conditions: vec![
                    WaitCondition::Effect(first.clone()),
                    WaitCondition::Effect(second.clone()),
                ],
            },
        );

        assert_eq!(table.get("root").unwrap().state, TaskLifecycle::Suspended);
        assert_eq!(
            table.notify(&WaitKey::Effect(first.clone())),
            Vec::<TaskId>::new()
        );
        assert_eq!(
            table
                .get("root")
                .unwrap()
                .wait_set
                .as_ref()
                .unwrap()
                .satisfied
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            vec![0],
            "partial All progress is durable task state, not reverse-index state"
        );

        assert_eq!(
            table.notify(&WaitKey::Effect(first)),
            Vec::<TaskId>::new(),
            "a duplicate event cannot satisfy the missing condition"
        );
        assert_eq!(
            table.notify(&WaitKey::Effect(second)),
            vec![TaskId::from("root")]
        );
        assert_eq!(table.get("root").unwrap().state, TaskLifecycle::Ready);
        assert!(table.get("root").unwrap().wait_set.is_none());
        assert_eq!(
            table.notify(&WaitKey::Effect(
                crate::runtime::kernel::wire::EffectId::new("effect-2").unwrap()
            )),
            Vec::<TaskId>::new(),
            "a satisfied WaitSet wakes exactly once"
        );
    }

    #[test]
    fn terminal_waiter_is_removed_without_becoming_runnable() {
        use crate::scheduler::wait_index::WaitKey;

        let mut table = TaskTable::new();
        table.insert(Tcb::root("root", SchedulerBudget::default()));
        let effect = crate::runtime::kernel::wire::EffectId::new("effect-terminal").unwrap();
        table.register_wait_set(
            "root",
            WaitSet {
                mode: WaitMode::Any,
                conditions: vec![WaitCondition::Effect(effect.clone())],
            },
        );
        table.get_mut("root").unwrap().state = TaskLifecycle::Done(TerminationReason::UserAbort);

        assert!(table.notify(&WaitKey::Effect(effect.clone())).is_empty());
        assert_eq!(
            table.get("root").unwrap().state,
            TaskLifecycle::Done(TerminationReason::UserAbort)
        );
        assert!(table.get("root").unwrap().wait_set.is_none());
        assert!(
            table
                .wait_index()
                .lookup(&WaitKey::Effect(effect))
                .is_empty()
        );
    }

    #[test]
    fn wake_order_is_identical_before_and_after_reverse_index_rebuild() {
        use crate::scheduler::wait_index::WaitKey;

        let mut live = TaskTable::new();
        live.insert(Tcb::root("root", SchedulerBudget::default()));
        live.insert(Tcb::root("task-a", SchedulerBudget::default()));
        live.insert(Tcb::root("task-b", SchedulerBudget::default()));
        let effect = crate::runtime::kernel::wire::EffectId::new("shared-effect").unwrap();

        // Registration order deliberately disagrees with task/checkpoint order. A rebuild only
        // has durable task rows available, so wake ordering cannot depend on this ephemeral order.
        live.wait_for_condition("task-b", &WaitCondition::Effect(effect.clone()));
        live.wait_for_condition("task-a", &WaitCondition::Effect(effect.clone()));
        let mut restored = live.clone();
        restored.rebuild_wait_index();

        let key = WaitKey::Effect(effect);
        let live_order = live.notify(&key);
        let restored_order = restored.notify(&key);
        assert_eq!(live_order, restored_order);
        assert_eq!(
            live_order,
            vec![TaskId::from("task-a"), TaskId::from("task-b")]
        );
    }

    #[test]
    fn terminal_legacy_waiter_loses_all_wait_state_without_resuming() {
        use crate::scheduler::wait_index::WaitKey;

        let mut table = TaskTable::new();
        table.insert(Tcb::root("root", SchedulerBudget::default()));
        table.set_wait("root", Some(WaitReason::Approval));
        table.get_mut("root").unwrap().state = TaskLifecycle::Done(TerminationReason::UserAbort);

        let key = WaitKey::Approval(ApprovalId(CompactString::from("pending")));
        assert!(table.notify(&key).is_empty());
        let task = table.get("root").unwrap();
        assert_eq!(
            task.state,
            TaskLifecycle::Done(TerminationReason::UserAbort)
        );
        assert!(task.wait.is_none());
        assert!(task.wait_set.is_none());
        assert!(table.wait_index().lookup(&key).is_empty());
    }

    #[test]
    fn timer_wakes_exactly_at_deadline_not_before() {
        // `WaitReason`/`Tcb.wait` stays legacy-only (spc_003-01's explicit boundary: not
        // modified). `Timer` has no `WaitReason` counterpart, so the `WaitIndex` alone is the
        // source of truth for whether a task is still waiting on one.
        let mut table = TaskTable::new();
        table.insert(Tcb::root("root", SchedulerBudget::default()));
        table.wait_for_timer("root", LogicalDeadline(1_000));
        let key = crate::scheduler::wait_index::WaitKey::Timer(LogicalDeadline(1_000));
        assert_eq!(table.wait_index().lookup(&key), &[TaskId::from("root")]);

        let woken_early = table.wake_expired_timers(999);
        assert!(woken_early.is_empty(), "must not wake before the deadline");
        assert_eq!(table.wait_index().lookup(&key), &[TaskId::from("root")]);

        let woken = table.wake_expired_timers(1_000);
        assert_eq!(woken, vec![TaskId::from("root")]);
        assert!(table.wait_index().lookup(&key).is_empty());
    }

    #[test]
    fn set_wait_syncs_the_wait_index() {
        let mut table = TaskTable::new();
        table.insert(Tcb::root("root", SchedulerBudget::default()));

        table.set_wait("root", Some(WaitReason::Approval));
        assert_eq!(table.get("root").unwrap().wait, Some(WaitReason::Approval));
        let (condition, _mode) = <(WaitCondition, WaitMode)>::from(WaitReason::Approval);
        let key = crate::scheduler::wait_index::WaitKey::Approval(match condition {
            WaitCondition::Approval(id) => id,
            _ => unreachable!(),
        });
        assert_eq!(table.wait_index().lookup(&key), &[TaskId::from("root")]);

        table.set_wait("root", None);
        assert_eq!(table.get("root").unwrap().wait, None);
        assert!(table.wait_index().lookup(&key).is_empty());
    }

    #[test]
    fn wait_reason_approval_converts_to_single_approval_condition() {
        let (condition, mode) = <(WaitCondition, WaitMode)>::from(WaitReason::Approval);
        assert!(matches!(condition, WaitCondition::Approval(_)));
        assert_eq!(mode, WaitMode::Any);
    }

    #[test]
    fn wait_reason_sub_agent_join_converts_to_children_wait_all() {
        let ids = vec![TaskId::from("child-1"), TaskId::from("child-2")];
        let (condition, mode) =
            <(WaitCondition, WaitMode)>::from(WaitReason::SubAgentJoin(ids.clone()));
        assert_eq!(condition, WaitCondition::Children(ids));
        assert_eq!(mode, WaitMode::All);
    }

    #[test]
    fn wait_condition_and_wait_mode_variants_compile() {
        let conditions = vec![
            WaitCondition::Effect(crate::runtime::kernel::wire::EffectId::new("e1").unwrap()),
            WaitCondition::Child(TaskId::from("child-1")),
            WaitCondition::Children(vec![TaskId::from("child-1"), TaskId::from("child-2")]),
            WaitCondition::Approval(ApprovalId("approval-1".into())),
            WaitCondition::Signal(SignalFilter("topic:*".into())),
            WaitCondition::Timer(LogicalDeadline(1_000)),
            WaitCondition::Channel(ChannelId("chan-1".into())),
            WaitCondition::Resource(ResourceKey("res-1".into())),
            WaitCondition::External(SubscriptionId("sub-1".into())),
        ];
        for condition in &conditions {
            match condition {
                WaitCondition::Effect(_)
                | WaitCondition::Child(_)
                | WaitCondition::Children(_)
                | WaitCondition::Approval(_)
                | WaitCondition::Signal(_)
                | WaitCondition::Timer(_)
                | WaitCondition::Channel(_)
                | WaitCondition::Resource(_)
                | WaitCondition::External(_) => {}
            }
        }

        let wait_set = WaitSet {
            mode: WaitMode::Any,
            conditions,
        };
        assert_eq!(wait_set.mode, WaitMode::Any);
        assert_eq!(wait_set.conditions.len(), 9);
    }

    #[test]
    fn rebuild_children_recovers_lineage_after_out_of_order_restore() {
        // spc_002-09: checkpoint restore inserts tasks in whatever order the wire state lists
        // them — `TaskTable::insert`'s children-registration only fires when the parent is
        // already present, so a child row landing before its parent silently drops that edge.
        // `rebuild_children` must recover it regardless of insertion order.
        let mut table = TaskTable::new();
        let mut child = Tcb::root("child", SchedulerBudget::default());
        child.parent = Some(TaskId::from("root"));
        table.insert(child); // parent "root" does not exist yet at insertion time
        table.insert(Tcb::root("root", SchedulerBudget::default()));

        assert!(
            !table
                .get("root")
                .unwrap()
                .children
                .contains(&TaskId::from("child")),
            "sanity: out-of-order insert must NOT have already fixed itself"
        );

        table.rebuild_children();

        assert!(
            table
                .get("root")
                .unwrap()
                .children
                .contains(&TaskId::from("child"))
        );
    }

    #[test]
    fn rebuild_wait_index_recovers_a_restored_waits_index_entry() {
        // Mirrors what checkpoint restore actually does: build the Tcb with `.wait` already set
        // (as `restore_task_wait` + direct field assignment does in `driver.rs`), never through
        // `TaskTable::set_wait`. Before the fix, the WaitIndex has no record of this task at all.
        let mut table = TaskTable::new();
        let mut root = Tcb::root("root", SchedulerBudget::default());
        root.wait = Some(WaitReason::Approval);
        table.insert(root);

        let (condition, _mode) = <(WaitCondition, WaitMode)>::from(WaitReason::Approval);
        let key = crate::scheduler::wait_index::WaitKey::Approval(match condition {
            WaitCondition::Approval(id) => id,
            _ => unreachable!(),
        });
        assert!(
            table.wait_index().lookup(&key).is_empty(),
            "sanity: inserting a Tcb with .wait already set must NOT auto-sync WaitIndex"
        );

        table.rebuild_wait_index();

        assert_eq!(table.wait_index().lookup(&key), &[TaskId::from("root")]);
    }

    #[test]
    fn rebuild_wait_index_is_idempotent() {
        let mut table = TaskTable::new();
        let mut root = Tcb::root("root", SchedulerBudget::default());
        root.wait = Some(WaitReason::Approval);
        table.insert(root);

        table.rebuild_wait_index();
        table.rebuild_wait_index();

        let (condition, _mode) = <(WaitCondition, WaitMode)>::from(WaitReason::Approval);
        let key = crate::scheduler::wait_index::WaitKey::Approval(match condition {
            WaitCondition::Approval(id) => id,
            _ => unreachable!(),
        });
        assert_eq!(
            table.wait_index().lookup(&key),
            &[TaskId::from("root")],
            "calling rebuild_wait_index twice must not duplicate the entry"
        );
    }

    #[test]
    fn has_cycle_is_false_on_a_normal_tree() {
        let mut table = TaskTable::new();
        table.insert(Tcb::root("A", SchedulerBudget::default()));
        let b = Tcb::spawned_in(
            &manifest_for("B"),
            SchedulerBudget::default(),
            TaskLifecycle::Running,
            table.root_id(),
        );
        table.insert(b);
        let c = Tcb::spawned_in(
            &manifest_for("C"),
            SchedulerBudget::default(),
            TaskLifecycle::Running,
            Some(TaskId::from("B")),
        );
        table.insert(c);

        assert!(!table.has_cycle());
    }

    #[test]
    fn has_cycle_detects_a_manually_constructed_cycle() {
        // Detector-validity check only: the public spawn API can never produce this — a not-yet-
        // existing task cannot already be an ancestor's parent (spc_002-04 derives `parent` from a
        // structural fact, never from a future id).
        let mut table = TaskTable::new();
        table.insert(Tcb::root("A", SchedulerBudget::default()));
        let b = Tcb::spawned_in(
            &manifest_for("B"),
            SchedulerBudget::default(),
            TaskLifecycle::Running,
            table.root_id(),
        );
        table.insert(b);
        // Force a cycle: A's parent becomes B, even though B's parent is A.
        table.get_mut("A").unwrap().parent = Some(TaskId::from("B"));

        assert!(table.has_cycle());
    }

    #[test]
    fn supervision_policy_defaults_match_spec_section_4() {
        let root = Tcb::root("root", SchedulerBudget::default());
        assert_eq!(
            root.supervision.child_failure,
            ChildFailurePolicy::Propagate
        );
        assert_eq!(root.supervision.max_restarts, None);
        assert!(root.supervision.cancel_children_on_exit);

        let manifest = manifest_for("child");
        let child = Tcb::spawned_in(
            &manifest,
            SchedulerBudget::default(),
            TaskLifecycle::Running,
            Some(TaskId::from("root")),
        );
        assert_eq!(child.supervision, SupervisionPolicy::default());
    }

    #[test]
    fn spc_008_05_cancel_subtree_exempts_a_detached_child_and_its_own_descendants() {
        let mut table = TaskTable::new();
        table.insert(Tcb::root("A", SchedulerBudget::default()));
        let mut b = Tcb::spawned_in(
            &manifest_for("B"),
            SchedulerBudget::default(),
            TaskLifecycle::Running,
            table.root_id(),
        );
        b.detached = true;
        table.insert(b);
        let c = Tcb::spawned_in(
            &manifest_for("C"),
            SchedulerBudget::default(),
            TaskLifecycle::Running,
            Some(TaskId::from("B")),
        );
        table.insert(c);

        table.cancel_subtree("A");

        assert_eq!(
            table.get("A").unwrap().state,
            TaskLifecycle::Done(TerminationReason::UserAbort),
            "A itself must still be cancelled"
        );
        assert_eq!(
            table.get("B").unwrap().state,
            TaskLifecycle::Running,
            "detached B must not be cancelled by A's cancellation"
        );
        assert_eq!(
            table.get("C").unwrap().state,
            TaskLifecycle::Running,
            "C (B's own child) must not be reached either — detaching exempts the whole subtree"
        );
    }

    #[test]
    fn cancel_subtree_terminates_the_whole_tree() {
        let mut table = TaskTable::new();
        table.insert(Tcb::root("A", SchedulerBudget::default()));
        let b = Tcb::spawned_in(
            &manifest_for("B"),
            SchedulerBudget::default(),
            TaskLifecycle::Running,
            table.root_id(),
        );
        table.insert(b);
        let c = Tcb::spawned_in(
            &manifest_for("C"),
            SchedulerBudget::default(),
            TaskLifecycle::Running,
            Some(TaskId::from("B")),
        );
        table.insert(c);

        table.cancel_subtree("A");

        for id in ["A", "B", "C"] {
            assert_eq!(
                table.get(id).unwrap().state,
                TaskLifecycle::Done(TerminationReason::UserAbort),
                "task {id} should be cancelled"
            );
        }
    }

    #[test]
    fn cancel_subtree_on_a_leaf_only_cancels_itself() {
        let mut table = TaskTable::new();
        table.insert(Tcb::root("root", SchedulerBudget::default()));
        table.cancel_subtree("root");
        assert_eq!(
            table.get("root").unwrap().state,
            TaskLifecycle::Done(TerminationReason::UserAbort)
        );
    }

    #[test]
    fn spawn_registers_child_in_parent_children() {
        let mut table = TaskTable::new();
        table.insert(Tcb::root("root", SchedulerBudget::default()));
        let manifest = manifest_for("child-1");
        let child = Tcb::spawned_in(
            &manifest,
            SchedulerBudget::default(),
            TaskLifecycle::Running,
            table.root_id(),
        );
        table.insert(child.clone());

        assert!(
            table
                .get("root")
                .unwrap()
                .children
                .contains(&TaskId::from("child-1"))
        );

        // A second spawn accumulates rather than clobbering the first.
        let manifest_2 = manifest_for("child-2");
        let child_2 = Tcb::spawned_in(
            &manifest_2,
            SchedulerBudget::default(),
            TaskLifecycle::Running,
            table.root_id(),
        );
        table.insert(child_2);
        let root_children = &table.get("root").unwrap().children;
        assert!(root_children.contains(&TaskId::from("child-1")));
        assert!(root_children.contains(&TaskId::from("child-2")));
    }

    #[test]
    fn tcb_children_default_empty() {
        let tcb = Tcb::root("root", SchedulerBudget::default());
        assert!(tcb.children.is_empty());
    }

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

    #[test]
    fn spc_006_03_send_message_delivers_into_the_recipients_mailbox() {
        use crate::scheduler::mailbox::{LogicalTime, MailboxMessage, MessageId};
        use crate::types::signal::Urgency;

        let mut table = TaskTable::new();
        table.insert(Tcb::root("a", SchedulerBudget::default()));
        table.insert(Tcb::root("b", SchedulerBudget::default()));

        table.send_message(MailboxMessage {
            id: MessageId::from("msg-1"),
            from: TaskId::from("a"),
            to: TaskId::from("b"),
            kind: CompactString::from("research_result"),
            payload_handle: 1,
            priority: Urgency::Normal,
            timestamp: LogicalTime(0),
            expires_at: None,
        });

        let received = table
            .get_mut("b")
            .unwrap()
            .mailbox
            .receive()
            .expect("B must have received A's message");
        assert_eq!(received.from, TaskId::from("a"));
        assert_eq!(received.id, MessageId::from("msg-1"));
        assert!(table.get_mut("a").unwrap().mailbox.receive().is_none());
    }

    #[test]
    fn spc_006_03_send_message_to_an_unknown_task_is_dropped_not_panicking() {
        use crate::scheduler::mailbox::{LogicalTime, MailboxMessage, MessageId};
        use crate::types::signal::Urgency;

        let mut table = TaskTable::new();
        table.insert(Tcb::root("a", SchedulerBudget::default()));

        table.send_message(MailboxMessage {
            id: MessageId::from("msg-1"),
            from: TaskId::from("a"),
            to: TaskId::from("nobody"),
            kind: CompactString::from("kind"),
            payload_handle: 1,
            priority: Urgency::Normal,
            timestamp: LogicalTime(0),
            expires_at: None,
        });
        // No panic — the only observable behavior is that the message goes nowhere.
    }

    #[test]
    fn spc_019_08_local_ipc_derives_sender_and_wakes_mailbox_and_channel_waiters() {
        use crate::scheduler::mailbox::{LogicalTime, MailboxMessage};
        use crate::scheduler::wait_index::WaitKey;
        use crate::types::signal::Urgency;

        let mut table = TaskTable::new();
        table.insert(Tcb::root("a", SchedulerBudget::default()));
        table.insert(Tcb::root("b", SchedulerBudget::default()));
        let mailbox_subscription = SubscriptionId("mailbox:b".into());
        table.wait_for_condition("b", &WaitCondition::External(mailbox_subscription.clone()));
        let message = MailboxMessage {
            id: "m1".into(),
            from: "forged".into(),
            to: "b".into(),
            kind: "result".into(),
            payload_handle: 7,
            priority: Urgency::Normal,
            timestamp: LogicalTime(1),
            expires_at: None,
        };
        assert_eq!(
            table.send_message_from("a", message.clone(), LogicalTime(1)),
            Ok(true)
        );
        assert_eq!(table.get("b").unwrap().state, TaskLifecycle::Ready);
        assert!(
            table
                .wait_index()
                .lookup(&WaitKey::External(mailbox_subscription))
                .is_empty()
        );
        let received = table.receive_mailbox("b", LogicalTime(1), 1).unwrap();
        assert_eq!(received[0].from.as_str(), "a");
        assert_eq!(
            table.send_message_from("a", message, LogicalTime(1)),
            Ok(false),
            "redelivery is a durable no-op"
        );

        let channel_id = ChannelId("results".into());
        table.wait_for_condition("b", &WaitCondition::Channel(channel_id.clone()));
        assert_eq!(
            table.publish_channel(
                "a",
                channel_id.clone(),
                vec!["b".into()],
                MailboxMessage {
                    id: "cm1".into(),
                    to: "ignored".into(),
                    ..received[0].clone()
                },
                LogicalTime(1),
            ),
            Ok(true)
        );
        assert_eq!(table.get("b").unwrap().state, TaskLifecycle::Ready);
        assert_eq!(
            table
                .receive_channel("b", &channel_id, LogicalTime(1))
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn spc_019_09_object_registration_wakes_resource_wait_and_receiver_capability_gates_read() {
        use crate::mm::handle::{Handle, HandleKind, ObjectDescriptor, object_access_allowed};
        use crate::types::capability::{
            ActionSet, Capability, CapabilityId, CapabilityKind, ConstraintSet, Principal,
            ResourceSelector,
        };

        let mut table = TaskTable::new();
        table.insert(Tcb::root("a", SchedulerBudget::default()));
        table.insert(Tcb::root("b", SchedulerBudget::default()));
        let resource = ResourceKey("object:a/7".into());
        table.wait_for_condition("b", &WaitCondition::Resource(resource));
        let descriptor = ObjectDescriptor::from_handle(
            "a".into(),
            &Handle::resident(7, HandleKind::ToolResult, 10),
            1,
        );
        assert_eq!(table.register_object("a", descriptor.clone()), Ok(true));
        assert_eq!(table.get("b").unwrap().state, TaskLifecycle::Ready);
        assert!(!object_access_allowed(
            &table.get("b").unwrap().capabilities,
            "read",
            &descriptor
        ));

        table.get_mut("b").unwrap().capabilities = vec![Capability {
            id: CapabilityId("read-a-7".into()),
            kind: CapabilityKind::Tool,
            resource: ResourceSelector("object:a/7".into()),
            actions: ActionSet(["read".into()].into_iter().collect()),
            constraints: ConstraintSet::default(),
            lease: None,
            delegatable: false,
            issuer: Principal("a".into()),
        }];
        assert!(object_access_allowed(
            &table.get("b").unwrap().capabilities,
            "read",
            &descriptor
        ));
        assert_eq!(table.register_object("a", descriptor), Ok(false));
    }

    #[test]
    fn spc_019_11_workflow_nested_timer_and_message_work_share_one_stable_trace() {
        use crate::scheduler::runnable::{LocalRunnable, LocalRunnableKind, order_runnables};

        let mut table = TaskTable::new();
        table.insert(Tcb::root("root", SchedulerBudget::default()));
        table.insert(Tcb::spawned_in(
            &manifest_for("nested"),
            SchedulerBudget::default(),
            TaskLifecycle::Ready,
            Some("root".into()),
        ));
        table.insert(Tcb::spawned_in(
            &manifest_for("timer"),
            SchedulerBudget::default(),
            TaskLifecycle::Running,
            Some("root".into()),
        ));
        table.insert(Tcb::spawned_in(
            &manifest_for("mail"),
            SchedulerBudget::default(),
            TaskLifecycle::Running,
            Some("root".into()),
        ));
        table.wait_for_timer("timer", LogicalDeadline(10));
        table.wait_for_condition(
            "mail",
            &WaitCondition::External(SubscriptionId("mailbox:mail".into())),
        );
        table.wake_expired_timers(10);
        table.notify(&super::super::wait_index::WaitKey::External(
            SubscriptionId("mailbox:mail".into()),
        ));

        let mut candidates = table.runnable_candidates();
        candidates.push(LocalRunnable::workflow("wf-node2", 0));
        let trace = order_runnables(candidates);
        assert_eq!(
            trace
                .iter()
                .map(|entry| (entry.id.as_str(), entry.kind))
                .collect::<Vec<_>>(),
            vec![
                ("mail", LocalRunnableKind::MessageWaiter),
                ("nested", LocalRunnableKind::NestedTask),
                ("timer", LocalRunnableKind::TimerWaiter),
                ("wf-node2", LocalRunnableKind::WorkflowNode),
            ]
        );

        let mut restored = table.clone();
        restored.rebuild_wait_index();
        let mut restored_candidates = restored.runnable_candidates();
        restored_candidates.push(LocalRunnable::workflow("wf-node2", 0));
        assert_eq!(order_runnables(restored_candidates), trace);
    }
}
