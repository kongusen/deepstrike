//! Root entry, execution focus and the logical payloads a root start carries (spec §7.4).

use serde::{Deserialize, Serialize};

use super::scalar::{BoundedJson, CallId, NodeId, TaskId, WireU64, WorkflowId};

// ---------------------------------------------------------------------------------------------
// root entry
// ---------------------------------------------------------------------------------------------

/// The **only** way an operation starts (§7.4). There is no second start shape: no generic
/// `Resume`, no root `LoadWorkflow` that first pretends to be an agent run, no host-minted
/// sub-agent spawn.
///
/// * an agent root goes straight to a provider call;
/// * a workflow root builds its DAG and spawns tasks directly, and its completion commits the
///   root terminal itself — nothing external "completes the run".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RootEntry {
    Agent(RootAgentEntry),
    Workflow(RootWorkflowEntry),
}

impl RootEntry {
    pub fn root_kind(&self) -> RootKind {
        match self {
            Self::Agent(_) => RootKind::Agent,
            Self::Workflow(_) => RootKind::Workflow,
        }
    }
}

/// Agent root. `initial_context` is deliberately absent: it belongs to `StartOperation` and is
/// never duplicated per variant.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RootAgentEntry {
    pub task: LogicalTask,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_spec: Option<LogicalAgentSpec>,
}

/// Workflow root.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RootWorkflowEntry {
    pub spec: WorkflowSpec,
}

/// Immutable for the whole operation lifetime (§6.1.5/§6.1.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RootKind {
    Agent,
    Workflow,
}

/// Where the operation's control flow currently is.
///
/// GAP-1 fixes a **closed** transition table; the transitions themselves are implemented with the
/// root execution work (Phase 3), and this type is what they are allowed to express:
///
/// * `RootKind::Agent` starts at `AgentTurn { root task }`. Exactly two transitions exist:
///   when the workflow an agent started through a P1 syscall **commits** its start effect, focus
///   moves to `WorkflowController { workflow_id, parent_task_id: Some(agent task) }`; when that
///   workflow completes (success, failure or cancellation) and the completion **commits**, focus
///   moves back to the original `AgentTurn`. Depth is at most 1 — workflows have no stack
///   (§10.2), so requesting another workflow while the focus is a `WorkflowController` is an
///   `InvalidAuthority` fault.
/// * `RootKind::Workflow` is permanently `WorkflowController { root workflow, parent_task_id:
///   None }`. Agent execution inside a DAG node is a P2 child attempt and does not move the
///   root's focus.
/// * Focus only ever moves on a **committed** transition. There is no input — host command or
///   otherwise — that sets it directly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExecutionFocus {
    AgentTurn(AgentTurnFocus),
    WorkflowController(WorkflowControllerFocus),
}

/// Newtype payloads rather than inline variants: `deny_unknown_fields` does not apply to an
/// inline struct variant of an internally tagged enum.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentTurnFocus {
    pub task_id: TaskId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowControllerFocus {
    pub workflow_id: WorkflowId,
    /// `Some` ⇒ a nested workflow inside an agent root; its completion restores the parent agent
    /// instead of committing a root terminal. `None` ⇒ the root workflow itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_task_id: Option<TaskId>,
}

impl ExecutionFocus {
    pub fn agent_turn(task_id: TaskId) -> Self {
        Self::AgentTurn(AgentTurnFocus { task_id })
    }

    pub fn workflow_controller(workflow_id: WorkflowId, parent_task_id: Option<TaskId>) -> Self {
        Self::WorkflowController(WorkflowControllerFocus {
            workflow_id,
            parent_task_id,
        })
    }

    /// The root kind this focus is only ever reachable from. A `WorkflowController` with a parent
    /// task is the nested case of an `Agent` root, so callers that need to distinguish "nested"
    /// from "root workflow" use [`Self::is_nested_in_agent`].
    pub fn root_kind_hint(&self) -> RootKind {
        match self {
            Self::AgentTurn(_) => RootKind::Agent,
            Self::WorkflowController(_) => RootKind::Workflow,
        }
    }

    /// Whether this focus is the nested workflow of an agent root — the only case in which a
    /// workflow completion must restore a parent agent rather than terminate the operation.
    pub fn is_nested_in_agent(&self) -> bool {
        matches!(
            self,
            Self::WorkflowController(WorkflowControllerFocus {
                parent_task_id: Some(_),
                ..
            })
        )
    }
}

// ---------------------------------------------------------------------------------------------
// logical payloads
// ---------------------------------------------------------------------------------------------

/// What the operation is trying to achieve. Purely logical: no session, no path, no host handle.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogicalTask {
    pub goal: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub criteria: Vec<String>,
    /// Free-form host label, carried through untouched — the kernel attaches no scheduling
    /// semantics to it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lane: Option<String>,
    #[serde(default, skip_serializing_if = "BoundedJson::is_null")]
    pub metadata: BoundedJson,
}

impl LogicalTask {
    pub fn new(goal: impl Into<String>) -> Self {
        Self {
            goal: goal.into(),
            criteria: Vec::new(),
            lane: None,
            metadata: BoundedJson::null(),
        }
    }
}

/// How the root agent should run — a **wire DTO of its own**, not the SDK's `AgentRunSpec`.
///
/// The SDK spec carries an `AgentIdentity` with `session_id`/`parent_session_id`: host storage
/// identity that the kernel must never learn, must never persist in a record, and must never be
/// able to correlate across operations. The logical spec carries only what changes kernel
/// decisions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogicalAgentSpec {
    pub goal: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<AgentRole>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub isolation: Option<AgentIsolation>,
    /// Logical context carried into a workflow child. This is operation-local content selection,
    /// not a host session reference, so it is safe to persist and replay in canonical records.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_inheritance: Option<LogicalContextInheritance>,
    /// Logical id of a verification contract in the operation's catalog.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_contract_id: Option<String>,
    #[serde(default, skip_serializing_if = "CapabilityFilter::is_empty")]
    pub capability_filter: CapabilityFilter,
    /// Pre-activation tool surface *under* the capability ceiling. `None` ⇒ the ceiling itself;
    /// `Some([])` is a legitimate, distinct minimal surface.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exposure_baseline: Option<Vec<String>>,
    /// Pure logical pacing policy for one loop round. It contains no host/session identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loop_round: Option<LogicalLoopRoundSpec>,
    #[serde(default, skip_serializing_if = "BoundedJson::is_null")]
    pub metadata: BoundedJson,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogicalLoopRoundSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_rounds: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_sleep_ms: Option<WireU64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_sleep_ms: Option<WireU64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_action: Option<String>,
}

impl LogicalAgentSpec {
    pub fn new(goal: impl Into<String>) -> Self {
        Self {
            goal: goal.into(),
            role: None,
            isolation: None,
            context_inheritance: None,
            verification_contract_id: None,
            capability_filter: CapabilityFilter::default(),
            exposure_baseline: None,
            loop_round: None,
            metadata: BoundedJson::null(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRole {
    Explore,
    Plan,
    Implement,
    Verify,
    Custom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentIsolation {
    Shared,
    ReadOnly,
    Worktree,
    Remote,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogicalContextInheritance {
    None,
    SystemOnly,
    Full,
}

/// Capability ceiling for a run. Empty on an axis ⇒ that axis does not narrow anything.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityFilter {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_kinds: Vec<CapabilityKind>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_ids: Vec<String>,
}

impl CapabilityFilter {
    pub fn is_empty(&self) -> bool {
        self.allowed_kinds.is_empty() && self.allowed_ids.is_empty()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityKind {
    Tool,
    Skill,
    Memory,
    Knowledge,
    McpServer,
    Command,
    Agent,
}

/// A workflow DAG as the host or an agent declares it.
///
/// The node/edge vocabulary converges with the root-execution and dynamic-append work (Task 9 /
/// Task 10). What Task 3 fixes is that a workflow root enters through [`RootEntry::Workflow`]
/// and carries no host-authored recovery state, because recovery is the kernel checkpoint's job
/// (§12.4).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowSpec {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub nodes: Vec<WorkflowNode>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowNode {
    pub node_id: NodeId,
    pub task: LogicalTask,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<NodeId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_spec: Option<LogicalAgentSpec>,
}

/// What the kernel needs to start thinking, and nothing else (§7.4).
///
/// Explicitly **not** here: session logs, provider replay envelopes, host paths. Those are host
/// storage concerns; a kernel that accepted them would be persisting facts it cannot reproduce.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InitialContext {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub messages: Vec<LogicalMessage>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub knowledge: Vec<KnowledgeEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<CapabilityGrant>,
    /// spc_009-04: the operation root's own fine-grained `Capability` set (resource/actions/
    /// constraints/lease) — distinct from `capabilities` above, which is the coarse kind/id
    /// allowlist `CapabilityGrant` predates this field with. The two do not merge: `CapabilityGrant`
    /// cannot be mechanically converted into a `Capability` (an allowed id implies nothing about
    /// what actions should be granted), so a Host that wants root to hold real delegatable
    /// authority must declare it here explicitly.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requested_capabilities: Vec<crate::types::capability::Capability>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogicalMessage {
    pub role: MessageRole,
    pub content: String,
    /// Host-observed token count. Absent ⇒ the kernel estimates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<u32>,
    /// Set on a tool message to pair it with its call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<CallId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    System,
    User,
    Assistant,
    Tool,
}

/// One knowledge-partition entry. `key` gives it identity (upsert semantics); `pinned` exempts it
/// from the knowledge budget sweep.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeEntry {
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pinned: bool,
}

/// A capability the operation may use, by logical identity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityGrant {
    pub kind: CapabilityKind,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// A capability reference used when withdrawing a grant.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityRef {
    pub kind: CapabilityKind,
    pub id: String,
}
