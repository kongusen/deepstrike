//! W0: a kernel-resident workflow run — the DAG state for one in-flight [`WorkflowSpec`].
//!
//! Pure data + pure advance logic, no I/O and no syscall: the [`crate::scheduler::state_machine::
//! LoopStateMachine`] drives this, gating each ready node's spawn through
//! `evaluate_syscall(Syscall::Spawn)` and reusing the existing batch-await barrier
//! (`SuspendState::SubAgentAwait`). This module only tracks *which* nodes are ready, spawned,
//! complete, partially complete, failed, or skipped, and builds each node's [`IsolationManifest`].
//!
//! Lifecycle: `ready_batch()` → (gate each) `mark_spawned` / `mark_denied` → on completion
//! `record_completion` → repeat until `is_complete()`.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::{DependencyContext, DependencyPolicy, NodeTrust, WorkflowNode, WorkflowSpec};
use crate::orchestration::task_graph::{SchedulingFactors, TaskGraph, TaskStatus};
use crate::types::agent::{AgentIsolation, AgentRole, ContextInheritance, IsolationManifest};
use crate::types::error::DeepStrikeError;
use crate::types::error::Result;
use crate::types::result::{LoopResult, TerminationReason};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowSubmissionError {
    pub node_index: usize,
    pub reason: String,
}

impl std::fmt::Display for WorkflowSubmissionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "node {}: {}", self.node_index, self.reason)
    }
}

impl std::error::Error for WorkflowSubmissionError {}

/// Terminal state of one workflow node. Non-terminal graph states are never exposed as outcomes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowNodeStatus {
    Completed,
    CompletedPartial,
    Failed,
    SkippedUpstreamFailed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowNodeOutcome {
    pub node_id: String,
    pub status: WorkflowNodeStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub termination: Option<TerminationReason>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<crate::types::message::CoreMessage>,
}

/// Deterministic kernel agent id for a workflow node (stable across resume / audit).
pub fn node_agent_id(node: usize) -> String {
    format!("wf-node{node}")
}

/// Enough to run one spawned workflow node, carried to the SDK in the `WorkflowBatchSpawned`
/// observation. Role/isolation/inheritance are canonical snake_case strings (serde names) so the
/// host SDK can rebuild an agent run spec — the kernel generates these specs internally, so this
/// is how the goal reaches the SDK that actually executes the node.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkflowSpawnInfo {
    pub agent_id: String,
    pub goal: String,
    pub role: String,
    pub isolation: String,
    pub context_inheritance: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_hint: Option<String>,
    /// W3 trust level (`"trusted"` | `"quarantined"`) — the SDK runs quarantined nodes without
    /// privileges and crosses their output back only as a structured summary.
    #[serde(default = "default_trust")]
    pub trust: String,
    /// G3 structured output: the JSON Schema the node's output must conform to, carried verbatim
    /// from [`WorkflowNode::output_schema`]. The SDK instructs the agent with it and validates +
    /// retries on its result. `None` when the node declared no schema. Additive ABI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<serde_json::Value>,
    /// The dependency agent ids (its `depends_on`, resolved to stable agent ids), so the SDK can
    /// put the dependency outputs in the node's context — a DAG edge carries data, not just
    /// ordering. Empty for a node with no dependencies.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input_agent_ids: Vec<String>,
    /// How the dependency outputs are put in the node's brief.
    #[serde(default, skip_serializing_if = "DependencyContext::is_default")]
    pub dependency_context: DependencyContext,
    /// M4/G5: the node's per-node cumulative token cap, if set. The SDK sets the child run's
    /// `max_total_tokens` to this so the node self-terminates at the cap. Additive ABI: omitted when
    /// `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_budget: Option<u64>,
    /// O3: per-node turn cap → the child run's `max_turns`. Additive ABI: omitted when `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_turns: Option<u32>,
    /// O3: per-node wall-clock cap (ms) → the child run's timeout. Additive ABI: omitted when `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_wall_ms: Option<u64>,
}

fn default_trust() -> String {
    "trusted".to_string()
}

/// G4 budget-as-signal: a snapshot of the workflow's remaining headroom under the active resource
/// quota, carried to the SDK on every `WorkflowBatchSpawned`. A coordinator/submitter node reads it
/// to *scale its next submission to what is actually available* — the analogue of the host-side
/// `budget.remaining()` in the code-orchestration model — instead of blindly hitting the cap and
/// eating a `Deny`. `None` remaining fields mean that dimension is unbounded (no quota set).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct WorkflowBudget {
    /// Nodes currently in the DAG (spec + every runtime submission so far).
    pub nodes_used: usize,
    /// `ResourceQuota::max_workflow_nodes`, if set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nodes_max: Option<usize>,
    /// `nodes_max - nodes_used` (saturating), if a node cap is set — how many more nodes may be
    /// submitted before the `max_workflow_nodes` backstop denies further growth.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nodes_remaining: Option<usize>,
    /// Sub-agents currently in the `running` state.
    pub running_subagents: usize,
    /// `ResourceQuota::max_concurrent_subagents`, if set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_concurrent_subagents: Option<usize>,
    /// `max_concurrent_subagents - running_subagents` (saturating), if a concurrency cap is set —
    /// how many of a submission's nodes can spawn *immediately* rather than deferring for a slot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub concurrency_remaining: Option<usize>,
    /// M4/G5: cumulative tokens spent across the run so far (the scheduler's `total_tokens`).
    /// `#[serde(default)]` keeps older JSON (without this field) deserializing to 0 — additive ABI.
    #[serde(default)]
    pub tokens_used: u64,
    /// M4/G5: `SchedulerBudget::max_total_tokens` — the run's cumulative token cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens_max: Option<u64>,
    /// M4/G5: `tokens_max - tokens_used` (saturating) — how many tokens remain before the run-level
    /// token budget terminates the workflow. Lets a coordinator scale its next submission to token
    /// headroom (the analogue of "use 10k tokens").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens_remaining: Option<u64>,
}

fn role_label(role: AgentRole) -> &'static str {
    match role {
        AgentRole::Explore => "explore",
        AgentRole::Plan => "plan",
        AgentRole::Implement => "implement",
        AgentRole::Verify => "verify",
        AgentRole::Custom => "custom",
    }
}

fn isolation_label(isolation: AgentIsolation) -> &'static str {
    match isolation {
        AgentIsolation::Shared => "shared",
        AgentIsolation::ReadOnly => "read_only",
        AgentIsolation::Worktree => "worktree",
        AgentIsolation::Remote => "remote",
    }
}

fn inheritance_label(inheritance: ContextInheritance) -> &'static str {
    match inheritance {
        ContextInheritance::None => "none",
        ContextInheritance::SystemOnly => "system_only",
        ContextInheritance::Full => "full",
    }
}

fn trust_label(trust: NodeTrust) -> &'static str {
    match trust {
        NodeTrust::Trusted => "trusted",
        NodeTrust::Quarantined => "quarantined",
    }
}

/// The state of one in-flight workflow execution.
pub struct WorkflowRun {
    graph: TaskGraph,
    nodes: Vec<WorkflowNode>,
    scheduler_policy: crate::scheduler::policy::SchedulerPolicyConfig,
    /// Completed-event lookup: kernel agent id → DAG node index.
    node_of_agent: HashMap<String, usize>,
}

/// Semantic workflow-node state used by logical checkpoint projection.
///
/// This is deliberately not serializable and exposes none of `TaskGraph`'s private indexes. The
/// checkpoint layer maps it onto its own stable DTO; restore rebuilds the indexes from this state.
#[derive(Debug, Clone)]
pub(crate) struct WorkflowRuntimeNodeState {
    pub status: TaskStatus,
    pub result: Option<LoopResult>,
    pub active_agent_id: Option<String>,
}

impl WorkflowRun {
    /// Build from a spec. Validates dependency indices + acyclicity (reuses `WorkflowSpec`).
    pub fn new(spec: &WorkflowSpec) -> Result<Self> {
        let mut run = Self {
            graph: spec.validate()?,
            nodes: spec.nodes.clone(),
            scheduler_policy: crate::scheduler::policy::SchedulerPolicyConfig::default(),
            node_of_agent: HashMap::new(),
        };
        run.refresh_scheduling();
        run.resolve_dependency_outcomes();
        Ok(run)
    }

    /// Rebuild an active run from the semantic state carried by a logical checkpoint.
    pub(crate) fn restore_from_checkpoint(
        spec: &WorkflowSpec,
        states: &[WorkflowRuntimeNodeState],
    ) -> Result<Self> {
        let mut run = Self::new(spec)?;
        let graph_states: Vec<(TaskStatus, Option<LoopResult>)> = states
            .iter()
            .map(|state| (state.status, state.result.clone()))
            .collect();
        run.graph
            .restore_runtime_state(&graph_states)
            .map_err(DeepStrikeError::InvalidConfig)?;

        for (node, state) in states.iter().enumerate() {
            match (state.status, state.active_agent_id.as_deref()) {
                (TaskStatus::Running, Some(agent_id)) => {
                    run.node_of_agent.insert(agent_id.to_string(), node);
                }
                (TaskStatus::Running, None) => {
                    return Err(DeepStrikeError::InvalidConfig(format!(
                        "running workflow node {node} carries no active agent id"
                    )));
                }
                (_, Some(agent_id)) => {
                    return Err(DeepStrikeError::InvalidConfig(format!(
                        "non-running workflow node {node} carries active agent id {agent_id:?}"
                    )));
                }
                (_, None) => {}
            }
        }
        run.refresh_scheduling();
        Ok(run)
    }

    /// Node indices whose dependencies are satisfied and that have not yet started.
    pub fn ready_batch(&mut self) -> Vec<usize> {
        self.graph.ready_tasks()
    }

    /// Whether every workflow node has reached a terminal state.
    pub fn is_complete(&self) -> bool {
        self.graph.all_done()
    }

    pub fn set_scheduler_policy(
        &mut self,
        policy: crate::scheduler::policy::SchedulerPolicyConfig,
    ) {
        self.scheduler_policy = policy;
        self.refresh_scheduling();
    }

    fn refresh_scheduling(&mut self) {
        let token_costs: Vec<u64> = self
            .nodes
            .iter()
            .map(|node| u64::from(node.token_budget.unwrap_or(0)))
            .collect();
        let factors: Vec<SchedulingFactors> = self
            .nodes
            .iter()
            .map(|node| node.scheduling_factors)
            .collect();
        self.graph
            .configure_scheduling_with_factors(self.scheduler_policy, &token_costs, &factors);
    }

    /// The agent id for a node's spawn: the stable `wf-node{N}`.
    pub fn current_agent_id(&self, node: usize) -> String {
        node_agent_id(node)
    }

    /// Build the isolation manifest for a node's current spawn, preserving its explicit isolation +
    /// context-inheritance (the `AgentRunSpec`→`from_spec` path would overwrite these with
    /// role defaults). spc_008-01/02: `requested_capabilities`/`requested_budget` now flow from the
    /// node's own fields — `permitted_capability_ids` (the coarse tool-name-glob ceiling, a distinct
    /// pre-existing mechanism) is unrelated and still left to a later round.
    pub fn manifest_for(&self, node: usize) -> IsolationManifest {
        let n = &self.nodes[node];
        IsolationManifest {
            agent_id: self.current_agent_id(node).into(),
            role: n.role,
            isolation: n.isolation,
            context_inheritance: n.context_inheritance,
            permitted_capability_ids: Vec::new(),
            requested_capabilities: n.requested_capabilities.clone(),
            requested_budget: n.requested_budget,
        }
    }

    /// The goal text for a node (for the spawn's run spec / context injection).
    /// W3 quarantine invariant: a quarantined node reads untrusted content and must run read-only.
    /// Returns `true` if the node is `Quarantined` yet declares a write-capable isolation
    /// (`Shared`/`Worktree`/`Remote`) — a privilege contradiction the kernel refuses to spawn,
    /// turning the SDK's "self-discipline" quarantine into an in-kernel, auditable enforcement.
    pub fn quarantine_violation(&self, node: usize) -> bool {
        let n = &self.nodes[node];
        matches!(n.trust, NodeTrust::Quarantined)
            && !matches!(n.isolation, AgentIsolation::ReadOnly)
    }

    /// The SDK-facing spawn descriptor for a node (agent id + goal + canonical role/isolation/
    /// inheritance strings + model hint). The kernel owns the spec; this is how the goal reaches
    /// the host that runs the node.
    pub fn spawn_info(&self, node: usize) -> WorkflowSpawnInfo {
        let n = &self.nodes[node];
        let input_agent_ids: Vec<String> = n.depends_on.iter().map(|&d| node_agent_id(d)).collect();
        WorkflowSpawnInfo {
            agent_id: self.current_agent_id(node),
            goal: n.task.goal.clone(),
            role: role_label(n.role).to_string(),
            isolation: isolation_label(n.isolation).to_string(),
            context_inheritance: inheritance_label(n.context_inheritance).to_string(),
            model_hint: n.model_hint.clone(),
            trust: trust_label(n.trust).to_string(),
            output_schema: n.output_schema.clone(),
            input_agent_ids,
            dependency_context: n.dependency_context,
            token_budget: n.token_budget,
            max_turns: n.max_turns,
            max_wall_ms: n.max_wall_ms,
        }
    }

    /// Mark a node as spawned: start it in the graph and map its kernel agent id back
    /// to the node for completion routing. (The live in-flight set is the executor's
    /// `SuspendState::SubAgentAwait.agent_ids` — the single source of in-flight truth.)
    pub fn mark_spawned(&mut self, node: usize, agent_id: &str) {
        self.graph.start(node);
        self.node_of_agent.insert(agent_id.to_string(), node);
    }

    /// Mark a node as denied by the syscall gate: fail it in the graph (dependents stay pending
    /// and will never become ready). Does not enter the live batch.
    pub fn mark_denied(&mut self, node: usize) {
        self.graph.fail(node);
        self.resolve_dependency_outcomes();
    }

    /// A host rejected or failed a spawn after the kernel reserved the node.
    /// Remove its completion route and fail the graph node so dependents cannot
    /// run on work that never started.
    pub fn mark_spawn_failed(&mut self, agent_id: &str) -> Option<usize> {
        let node = self.node_of_agent.remove(agent_id)?;
        self.graph.fail(node);
        self.resolve_dependency_outcomes();
        Some(node)
    }

    /// Record a completed sub-agent against its node. Returns the node index if `agent_id`
    /// belonged to this workflow, else `None`. Maps the run termination into an explicit node
    /// terminal state, then applies each dependent's declared policy.
    pub fn record_completion(&mut self, agent_id: &str, result: LoopResult) -> Option<usize> {
        let node = *self.node_of_agent.get(agent_id)?;
        self.settle_result(node, result);
        Some(node)
    }

    fn settle_result(&mut self, node: usize, result: LoopResult) {
        match result.termination {
            TerminationReason::Completed => self.graph.complete(node, result),
            TerminationReason::MaxTurns
            | TerminationReason::TokenBudget
            | TerminationReason::Timeout
            | TerminationReason::MilestoneExceeded
            | TerminationReason::ContextOverflow
            | TerminationReason::NoProgress => self.graph.complete_partial(node, result),
            TerminationReason::Error | TerminationReason::UserAbort => {
                self.graph.fail_with_result(node, result)
            }
        }
        self.resolve_dependency_outcomes();
    }

    /// Resolve every pending node whose dependency state is now decisive. Skips cascade until the
    /// graph reaches a fixed point, so a failed ancestor cannot leave hidden pending descendants.
    fn resolve_dependency_outcomes(&mut self) {
        loop {
            let mut changed = false;
            for node in 0..self.nodes.len() {
                if self.graph.get(node).map(|n| n.status) != Some(TaskStatus::Pending) {
                    continue;
                }
                let policy = self.nodes[node].dep_policy;
                if policy == DependencyPolicy::Optional {
                    self.graph.set_ready(node);
                    changed = true;
                    continue;
                }
                let statuses: Vec<TaskStatus> = self.nodes[node]
                    .depends_on
                    .iter()
                    .filter_map(|&dep| self.graph.get(dep).map(|n| n.status))
                    .collect();
                let all_terminal = statuses.iter().all(|status| status.is_terminal());
                let impossible = match policy {
                    DependencyPolicy::AllSuccess => statuses.iter().any(|status| {
                        matches!(
                            status,
                            TaskStatus::CompletedPartial
                                | TaskStatus::Failed
                                | TaskStatus::SkippedUpstreamFailed
                        )
                    }),
                    DependencyPolicy::AcceptPartial => statuses.iter().any(|status| {
                        matches!(
                            status,
                            TaskStatus::Failed | TaskStatus::SkippedUpstreamFailed
                        )
                    }),
                    DependencyPolicy::AllTerminal | DependencyPolicy::Optional => false,
                };
                if impossible {
                    self.graph.skip_upstream_failed(node);
                    changed = true;
                } else if all_terminal {
                    self.graph.set_ready(node);
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
    }

    // ── R3-1: runtime node submission (true loop-until-done / dynamic fan-out) ────────────────────

    /// Append a batch of nodes to the in-flight DAG at runtime — the kernel side of the dynamic
    /// "submit nodes" capability. A running
    /// node, on completion, can ask for more work to be spawned: unknown-size discovery
    /// (loop-until-done) and per-item fan-out (e.g. a claim-extractor spawning one verifier per
    /// claim) both reduce to "append these nodes now".
    ///
    /// Each submitted node's `depends_on` is batch-relative: both forward and backward edges are
    /// accepted when the complete batch is acyclic. Validation precedes mutation, so a malformed
    /// reference rejects the entire batch atomically.
    ///
    /// Pure graph mutation: the caller (state machine) is responsible for routing the trigger
    /// through `evaluate_syscall` before calling this, keeping the kernel's zero-I/O contract.
    ///
    /// G1 no-privilege-escalation: when `submitter` names a [`NodeTrust::Quarantined`] node, every
    /// node in this submission is coerced to `Quarantined` before append. A quarantined agent read
    /// untrusted content (which may be adversarial), so the topology it asks for is itself untrusted:
    /// it must not be able to launch a *trusted* (or write-capable) child and thereby escape its
    /// sandbox. This is transitive taint — a quarantined origin's descendants inherit quarantine —
    /// the topological analogue of a process spawned by an untrusted process inheriting its label.
    /// Trusted (or absent) submitters pass through unchanged. The coercion is enforced here in the
    /// kernel rather than trusting the SDK, and composes with the spawn-time
    /// [`Self::quarantine_violation`] gate (a coerced node that also asked for write isolation is
    /// then denied at spawn).
    pub fn submit_nodes_from(
        &mut self,
        submitter: Option<&str>,
        mut nodes: Vec<WorkflowNode>,
    ) -> std::result::Result<Vec<usize>, WorkflowSubmissionError> {
        let submitter_quarantined = submitter.is_some_and(|s| self.is_agent_quarantined(s));
        if submitter_quarantined {
            for node in &mut nodes {
                node.trust = NodeTrust::Quarantined;
            }
        }
        self.submit_nodes(nodes)
    }

    pub fn submit_nodes(
        &mut self,
        mut nodes: Vec<WorkflowNode>,
    ) -> std::result::Result<Vec<usize>, WorkflowSubmissionError> {
        let base = self.nodes.len();
        let batch_len = nodes.len();
        for (node_index, node) in nodes.iter().enumerate() {
            for &dependency in &node.depends_on {
                if dependency >= batch_len {
                    return Err(WorkflowSubmissionError {
                        node_index,
                        reason: format!(
                            "dependency {dependency} out of range for batch of {batch_len} nodes"
                        ),
                    });
                }
                if dependency == node_index {
                    return Err(WorkflowSubmissionError {
                        node_index,
                        reason: "node depends on itself".to_string(),
                    });
                }
            }
        }

        if WorkflowSpec::new(nodes.clone()).validate().is_err() {
            return Err(WorkflowSubmissionError {
                node_index: 0,
                reason: "submission introduces a dependency cycle".to_string(),
            });
        }

        for node in &mut nodes {
            node.depends_on = node.depends_on.iter().map(|dep| base + dep).collect();
        }

        let mut ids = Vec::with_capacity(nodes.len());
        for node in nodes {
            let deps = node.depends_on.clone();
            let idx = self.graph.add(node.task.clone(), deps);
            debug_assert_eq!(idx, self.nodes.len(), "graph/nodes index drift");
            self.nodes.push(node);
            ids.push(idx);
        }
        self.refresh_scheduling();
        self.resolve_dependency_outcomes();
        Ok(ids)
    }

    /// Whether `agent_id` belongs to this workflow.
    pub fn owns_agent(&self, agent_id: &str) -> bool {
        self.node_of_agent.contains_key(agent_id)
    }

    /// Rebuild the launch descriptor for a still-running logical node after an attempt failure.
    pub(crate) fn spawn_info_for_agent(&self, agent_id: &str) -> Option<WorkflowSpawnInfo> {
        self.node_of_agent
            .get(agent_id)
            .copied()
            .map(|node| self.spawn_info(node))
    }

    /// R3-3: whether the node behind `agent_id` is `Quarantined` (it read untrusted content). The
    /// kernel uses this to label that node's output as untrusted-origin when it crosses into the
    /// trusted parent context — the provenance half of the cross-boundary contract (shaping the
    /// output into a structured summary stays the SDK's job; the kernel cannot inspect content).
    pub fn is_agent_quarantined(&self, agent_id: &str) -> bool {
        self.node_of_agent
            .get(agent_id)
            .is_some_and(|&node| matches!(self.nodes[node].trust, NodeTrust::Quarantined))
    }

    /// Test instrument: mark a spawned node quarantined after the fact.
    ///
    /// Production quarantine is declared on the spec (`WorkflowNode::quarantined`). The canonical
    /// wire `WorkflowNode` has no trust field yet — see the SPEC-ISSUE in the canonical driver — so
    /// tests of the quarantine refusal reach the state through here instead of through the wire.
    #[cfg(test)]
    pub(crate) fn quarantine_agent(&mut self, agent_id: &str) -> bool {
        match self.node_of_agent.get(agent_id).copied() {
            Some(node) => {
                self.nodes[node].trust = NodeTrust::Quarantined;
                true
            }
            None => false,
        }
    }

    /// Test instrument: true when no node is currently `Running` — the spawned batch has
    /// fully reported back. Derived from the graph; the executor's in-flight truth is
    /// `SuspendState::SubAgentAwait.agent_ids`.
    #[cfg(test)]
    pub(crate) fn batch_drained(&self) -> bool {
        !(0..self.graph.len()).any(|i| {
            matches!(
                self.graph.get(i).map(|n| &n.status),
                Some(crate::orchestration::task_graph::TaskStatus::Running)
            )
        })
    }

    /// Close a workflow and return exactly one typed terminal outcome per graph node.
    pub fn finish(&mut self) -> Vec<WorkflowNodeOutcome> {
        for node in 0..self.graph.len() {
            match self.graph.get(node).map(|node| node.status) {
                Some(TaskStatus::Pending | TaskStatus::Ready) => {
                    self.graph.skip_upstream_failed(node)
                }
                Some(TaskStatus::Running) => self.graph.fail(node),
                _ => {}
            }
        }
        self.node_outcomes()
    }

    pub fn node_outcomes(&self) -> Vec<WorkflowNodeOutcome> {
        (0..self.graph.len())
            .filter_map(|node| {
                let graph_node = self.graph.get(node)?;
                let status = match graph_node.status {
                    TaskStatus::Completed => WorkflowNodeStatus::Completed,
                    TaskStatus::CompletedPartial => WorkflowNodeStatus::CompletedPartial,
                    TaskStatus::Failed => WorkflowNodeStatus::Failed,
                    TaskStatus::SkippedUpstreamFailed => WorkflowNodeStatus::SkippedUpstreamFailed,
                    TaskStatus::Pending | TaskStatus::Ready | TaskStatus::Running => return None,
                };
                Some(WorkflowNodeOutcome {
                    node_id: node_agent_id(node),
                    status,
                    termination: graph_node.result.as_ref().map(|result| result.termination),
                    output: graph_node
                        .result
                        .as_ref()
                        .and_then(|result| result.final_message.clone()),
                })
            })
            .collect()
    }

    /// #2-B abort: produce the same typed terminal contract as ordinary completion. Running/ready
    /// nodes fail with `UserAbort`; nodes that never started are skipped behind the aborted graph.
    pub fn abort_outcomes(&self) -> Vec<WorkflowNodeOutcome> {
        (0..self.graph.len())
            .filter_map(|node| {
                let graph_node = self.graph.get(node)?;
                let (status, termination) = match graph_node.status {
                    TaskStatus::Completed => (
                        WorkflowNodeStatus::Completed,
                        graph_node.result.as_ref().map(|result| result.termination),
                    ),
                    TaskStatus::CompletedPartial => (
                        WorkflowNodeStatus::CompletedPartial,
                        graph_node.result.as_ref().map(|result| result.termination),
                    ),
                    TaskStatus::Pending | TaskStatus::SkippedUpstreamFailed => {
                        (WorkflowNodeStatus::SkippedUpstreamFailed, None)
                    }
                    TaskStatus::Ready | TaskStatus::Running | TaskStatus::Failed => (
                        WorkflowNodeStatus::Failed,
                        Some(
                            graph_node
                                .result
                                .as_ref()
                                .map_or(TerminationReason::UserAbort, |result| result.termination),
                        ),
                    ),
                };
                Some(WorkflowNodeOutcome {
                    node_id: node_agent_id(node),
                    status,
                    termination,
                    output: graph_node
                        .result
                        .as_ref()
                        .and_then(|result| result.final_message.clone()),
                })
            })
            .collect()
    }

    /// Total node count.
    pub fn len(&self) -> usize {
        self.graph.len()
    }

    /// Project the active DAG without exposing any private graph indexes.
    pub(crate) fn checkpoint_nodes(&self) -> Vec<WorkflowRuntimeNodeState> {
        (0..self.graph.len())
            .filter_map(|node| {
                let graph_node = self.graph.get(node)?;
                let active_agent_id = (graph_node.status == TaskStatus::Running)
                    .then(|| {
                        self.node_of_agent.iter().find_map(|(agent_id, &owner)| {
                            (owner == node).then(|| agent_id.clone())
                        })
                    })
                    .flatten();
                Some(WorkflowRuntimeNodeState {
                    status: graph_node.status,
                    result: graph_node.result.clone(),
                    active_agent_id,
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestration::workflow::fanout_synthesize;
    use crate::types::result::{LoopResult, TerminationReason};
    use crate::types::task::RuntimeTask;

    fn done() -> LoopResult {
        LoopResult {
            termination: TerminationReason::Completed,
            final_message: None,
            turns_used: 1,
            total_tokens_used: 0,
            pace_decision: None,
        }
    }

    fn terminated(termination: TerminationReason) -> LoopResult {
        LoopResult {
            termination,
            ..done()
        }
    }

    fn fanout2() -> WorkflowRun {
        // 2 workers (nodes 0,1) → synthesize (node 2, depends on both)
        let spec = fanout_synthesize(
            vec![RuntimeTask::new("w0"), RuntimeTask::new("w1")],
            RuntimeTask::new("synth"),
        );
        WorkflowRun::new(&spec).unwrap()
    }

    /// Mimic one executor spawn round on a `WorkflowRun`: mark every ready node spawned (mapping
    /// its current agent id). Returns the spawned `(node, id)`s.
    fn spawn_round(run: &mut WorkflowRun) -> Vec<(usize, String)> {
        let ready = run.ready_batch();
        let mut out = Vec::new();
        for node in ready {
            let id = run.current_agent_id(node);
            run.mark_spawned(node, &id);
            out.push((node, id));
        }
        out
    }

    #[test]
    fn first_batch_is_the_workers() {
        let mut run = fanout2();
        assert_eq!(run.ready_batch(), vec![0, 1]);
        assert_eq!(run.len(), 3);
        assert!(!run.is_complete());
    }

    // ── R3-1: runtime node submission ────────────────────────────────────────────────────────

    #[test]
    fn submit_nodes_appends_independent_nodes_ready_immediately() {
        use crate::orchestration::workflow::WorkflowNode;
        use crate::types::agent::AgentRole;

        let mut run = fanout2(); // nodes 0,1 (workers) → 2 (synth)
        assert_eq!(run.len(), 3);
        let ids = run
            .submit_nodes(vec![
                WorkflowNode::new(RuntimeTask::new("extra-a"), AgentRole::Implement),
                WorkflowNode::new(RuntimeTask::new("extra-b"), AgentRole::Implement),
            ])
            .unwrap();
        assert_eq!(ids, vec![3, 4], "appended after the existing 3 nodes");
        assert_eq!(run.len(), 5);
        let ready = run.ready_batch();
        assert!(
            ready.contains(&3) && ready.contains(&4),
            "submitted independent nodes are immediately ready: {ready:?}"
        );
    }

    #[test]
    fn submitted_nodes_must_complete_before_workflow_is_done() {
        use crate::orchestration::workflow::{WorkflowNode, WorkflowSpec};
        use crate::types::agent::AgentRole;

        // A single spawn node that, on completion, submits more work (loop-until-done shape).
        let spec = WorkflowSpec::new(vec![WorkflowNode::new(
            RuntimeTask::new("root"),
            AgentRole::Implement,
        )]);
        let mut run = WorkflowRun::new(&spec).unwrap();
        let id0 = run.current_agent_id(0);
        run.mark_spawned(0, &id0);
        run.record_completion(&id0, done());
        let ids = run
            .submit_nodes(vec![WorkflowNode::new(
                RuntimeTask::new("more"),
                AgentRole::Implement,
            )])
            .unwrap();
        assert_eq!(ids, vec![1]);
        assert!(
            !run.is_complete(),
            "not complete while the submitted node is pending"
        );
        let spawned = spawn_round(&mut run);
        assert_eq!(spawned, vec![(1usize, "wf-node1".to_string())]);
        run.record_completion("wf-node1", done());
        assert!(
            run.is_complete(),
            "complete once the submitted node finishes"
        );
    }

    #[test]
    fn output_schema_reaches_the_spawn_descriptor() {
        use crate::orchestration::workflow::{WorkflowNode, WorkflowSpec};
        use crate::types::agent::AgentRole;

        // G3: a node declaring an output schema carries it verbatim to the SDK spawn descriptor.
        let schema = serde_json::json!({
            "type": "object",
            "required": ["verdict"],
            "properties": { "verdict": { "type": "string" } }
        });
        let spec = WorkflowSpec::new(vec![
            WorkflowNode::new(RuntimeTask::new("judge"), AgentRole::Verify)
                .with_output_schema(schema.clone()),
        ]);
        let run = WorkflowRun::new(&spec).unwrap();
        let info = run.spawn_info(0);
        assert_eq!(info.output_schema.as_ref(), Some(&schema));

        // Full serde round-trip preserves it (additive ABI).
        let json = serde_json::to_string(&info).unwrap();
        let back: WorkflowSpawnInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(back.output_schema, Some(schema));

        // A node without a schema omits the field entirely on the wire.
        let plain = WorkflowSpec::new(vec![WorkflowNode::new(
            RuntimeTask::new("x"),
            AgentRole::Implement,
        )]);
        let plain_info = WorkflowRun::new(&plain).unwrap().spawn_info(0);
        assert!(plain_info.output_schema.is_none());
        assert!(
            !serde_json::to_string(&plain_info)
                .unwrap()
                .contains("output_schema")
        );
    }

    #[test]
    fn quarantined_submitter_taints_submitted_nodes() {
        use crate::orchestration::workflow::{WorkflowNode, WorkflowSpec};
        use crate::types::agent::AgentRole;

        // G1: a quarantined root reads untrusted content, then tries to submit a node it declares
        // "trusted" (and write-capable). The kernel must coerce that node to quarantined — a
        // quarantined origin cannot escalate its descendants out of the sandbox.
        let spec = WorkflowSpec::new(vec![
            WorkflowNode::new(RuntimeTask::new("read-untrusted"), AgentRole::Explore).quarantined(),
        ]);
        let mut run = WorkflowRun::new(&spec).unwrap();
        let id0 = run.current_agent_id(0);
        run.mark_spawned(0, &id0);
        run.record_completion(&id0, done());

        // Submitted node claims Trusted; the quarantined submitter cannot grant that.
        let ids = run
            .submit_nodes_from(
                Some(&id0),
                vec![WorkflowNode::new(
                    RuntimeTask::new("act"),
                    AgentRole::Implement,
                )],
            )
            .unwrap();
        assert_eq!(ids, vec![1]);
        let id1 = run.current_agent_id(1);
        run.mark_spawned(1, &id1);
        assert!(
            run.is_agent_quarantined(&id1),
            "submitted node inherits the submitter's quarantine (no escalation)"
        );

        // A trusted / unknown submitter does NOT coerce — only quarantined origins taint.
        let ids2 = run
            .submit_nodes_from(
                None,
                vec![WorkflowNode::new(
                    RuntimeTask::new("trusted-work"),
                    AgentRole::Implement,
                )],
            )
            .unwrap();
        let id2 = run.current_agent_id(ids2[0]);
        run.mark_spawned(ids2[0], &id2);
        assert!(
            !run.is_agent_quarantined(&id2),
            "no quarantined submitter ⇒ no coercion"
        );
    }

    #[test]
    fn submit_nodes_honors_batch_relative_backward_deps() {
        use crate::orchestration::workflow::{WorkflowNode, WorkflowSpec};
        use crate::types::agent::AgentRole;

        let spec = WorkflowSpec::new(vec![WorkflowNode::new(
            RuntimeTask::new("root"),
            AgentRole::Implement,
        )]);
        let mut run = WorkflowRun::new(&spec).unwrap();
        let id0 = run.current_agent_id(0);
        run.mark_spawned(0, &id0);
        run.record_completion(&id0, done());
        // [extractor @offset 0, dependent @offset 1 depends on 0].
        let ids = run
            .submit_nodes(vec![
                WorkflowNode::new(RuntimeTask::new("extractor"), AgentRole::Implement),
                WorkflowNode::new(RuntimeTask::new("dependent"), AgentRole::Implement)
                    .with_depends_on(vec![0]),
            ])
            .unwrap();
        assert_eq!(ids, vec![1, 2]);
        assert_eq!(
            run.ready_batch(),
            vec![1],
            "backward dep keeps the dependent pending"
        );
        run.mark_spawned(1, "wf-node1");
        run.record_completion("wf-node1", done());
        assert_eq!(
            run.ready_batch(),
            vec![2],
            "dependent unblocks after the extractor"
        );
    }

    #[test]
    fn submit_nodes_accepts_acyclic_forward_dependencies() {
        use crate::orchestration::workflow::{WorkflowNode, WorkflowSpec};
        use crate::types::agent::AgentRole;

        let spec = WorkflowSpec::new(vec![WorkflowNode::new(
            RuntimeTask::new("root"),
            AgentRole::Implement,
        )]);
        let mut run = WorkflowRun::new(&spec).unwrap();
        // Node 0 in the batch depends on node 1: a valid forward edge. Only node 1 starts.
        let ids = run
            .submit_nodes(vec![
                WorkflowNode::new(RuntimeTask::new("consumer"), AgentRole::Implement)
                    .with_depends_on(vec![1]),
                WorkflowNode::new(RuntimeTask::new("producer"), AgentRole::Implement),
            ])
            .unwrap();
        assert_eq!(ids, vec![1, 2]);
        assert_eq!(
            run.ready_batch(),
            vec![2, 0],
            "the producer is on the longer critical path and runs before the independent root"
        );
        run.mark_spawned(2, "wf-node2");
        run.record_completion("wf-node2", done());
        assert!(run.ready_batch().contains(&1));
    }

    #[test]
    fn submit_nodes_rejects_malformed_batches_atomically() {
        use crate::orchestration::workflow::WorkflowNode;
        use crate::types::agent::AgentRole;

        for nodes in [
            vec![
                WorkflowNode::new(RuntimeTask::new("bad-range"), AgentRole::Implement)
                    .with_depends_on(vec![1]),
            ],
            vec![
                WorkflowNode::new(RuntimeTask::new("self"), AgentRole::Implement)
                    .with_depends_on(vec![0]),
            ],
            vec![
                WorkflowNode::new(RuntimeTask::new("a"), AgentRole::Implement)
                    .with_depends_on(vec![1]),
                WorkflowNode::new(RuntimeTask::new("b"), AgentRole::Implement)
                    .with_depends_on(vec![0]),
            ],
        ] {
            let mut run = fanout2();
            let before = run.len();
            assert!(run.submit_nodes(nodes).is_err());
            assert_eq!(run.len(), before, "rejection must precede every mutation");
        }
    }

    #[test]
    fn synth_becomes_ready_only_after_both_workers() {
        let mut run = fanout2();
        for &n in &[0usize, 1usize] {
            let id = node_agent_id(n);
            run.mark_spawned(n, &id);
        }
        assert!(!run.batch_drained());
        // first worker completes → synth not ready yet, batch not drained
        assert_eq!(run.record_completion(&node_agent_id(0), done()), Some(0));
        assert!(!run.batch_drained());
        assert!(run.ready_batch().is_empty());
        // second worker completes → batch drained, synth now ready
        assert_eq!(run.record_completion(&node_agent_id(1), done()), Some(1));
        assert!(run.batch_drained());
        assert_eq!(run.ready_batch(), vec![2]);
        assert!(!run.is_complete());
        // spawn + complete synth → workflow complete
        run.mark_spawned(2, &node_agent_id(2));
        run.record_completion(&node_agent_id(2), done());
        assert!(run.is_complete());
    }

    #[test]
    fn denied_node_skips_dependents_and_closes_outcome() {
        let mut run = fanout2();
        // node 0 spawned + completes; node 1 denied by the gate
        run.mark_spawned(0, &node_agent_id(0));
        run.mark_denied(1);
        run.record_completion(&node_agent_id(0), done());
        assert!(run.batch_drained());
        assert!(run.ready_batch().is_empty());
        assert!(run.is_complete());
        let outcomes = run.finish();
        assert_eq!(outcomes.len(), 3);
        assert_eq!(outcomes[0].status, WorkflowNodeStatus::Completed);
        assert_eq!(outcomes[1].status, WorkflowNodeStatus::Failed);
        assert_eq!(
            outcomes[2].status,
            WorkflowNodeStatus::SkippedUpstreamFailed
        );
    }

    #[test]
    fn terminal_mapping_and_dependency_policies_are_explicit() {
        use crate::orchestration::workflow::{DependencyPolicy, WorkflowNode, WorkflowSpec};
        use crate::types::agent::AgentRole;

        let cases = [
            (TerminationReason::Completed, WorkflowNodeStatus::Completed),
            (
                TerminationReason::MaxTurns,
                WorkflowNodeStatus::CompletedPartial,
            ),
            (
                TerminationReason::TokenBudget,
                WorkflowNodeStatus::CompletedPartial,
            ),
            (
                TerminationReason::Timeout,
                WorkflowNodeStatus::CompletedPartial,
            ),
            (
                TerminationReason::ContextOverflow,
                WorkflowNodeStatus::CompletedPartial,
            ),
            (
                TerminationReason::NoProgress,
                WorkflowNodeStatus::CompletedPartial,
            ),
            (
                TerminationReason::MilestoneExceeded,
                WorkflowNodeStatus::CompletedPartial,
            ),
            (TerminationReason::Error, WorkflowNodeStatus::Failed),
            (TerminationReason::UserAbort, WorkflowNodeStatus::Failed),
        ];
        for (termination, expected) in cases {
            let spec = WorkflowSpec::new(vec![WorkflowNode::new(
                RuntimeTask::new("node"),
                AgentRole::Implement,
            )]);
            let mut run = WorkflowRun::new(&spec).unwrap();
            run.mark_spawned(0, "wf-node0");
            run.record_completion("wf-node0", terminated(termination));
            let outcome = run.finish().remove(0);
            assert_eq!(outcome.status, expected);
            assert_eq!(outcome.termination, Some(termination));
        }

        let spec = WorkflowSpec::new(vec![
            WorkflowNode::new(RuntimeTask::new("upstream"), AgentRole::Implement),
            WorkflowNode::new(RuntimeTask::new("strict"), AgentRole::Implement)
                .with_depends_on(vec![0]),
            WorkflowNode::new(RuntimeTask::new("partial-ok"), AgentRole::Implement)
                .with_depends_on(vec![0])
                .with_dependency_policy(DependencyPolicy::AcceptPartial),
        ]);
        let mut run = WorkflowRun::new(&spec).unwrap();
        run.mark_spawned(0, "wf-node0");
        run.record_completion("wf-node0", terminated(TerminationReason::Timeout));
        assert_eq!(run.ready_batch(), vec![2]);
        assert_eq!(
            run.node_outcomes()[1].status,
            WorkflowNodeStatus::SkippedUpstreamFailed
        );
    }

    #[test]
    fn all_terminal_and_optional_have_distinct_waiting_semantics() {
        use crate::orchestration::workflow::{DependencyPolicy, WorkflowNode, WorkflowSpec};
        use crate::types::agent::AgentRole;

        let spec = WorkflowSpec::new(vec![
            WorkflowNode::new(RuntimeTask::new("upstream"), AgentRole::Implement),
            WorkflowNode::new(RuntimeTask::new("cleanup"), AgentRole::Implement)
                .with_depends_on(vec![0])
                .with_dependency_policy(DependencyPolicy::AllTerminal),
            WorkflowNode::new(RuntimeTask::new("best-effort"), AgentRole::Implement)
                .with_depends_on(vec![0])
                .with_dependency_policy(DependencyPolicy::Optional),
        ]);
        let mut run = WorkflowRun::new(&spec).unwrap();
        assert_eq!(run.ready_batch(), vec![0, 2]);
        run.mark_spawned(0, "wf-node0");
        run.record_completion("wf-node0", terminated(TerminationReason::Error));
        assert!(run.ready_batch().contains(&1));
    }

    #[test]
    fn manifest_preserves_node_isolation_and_inheritance() {
        let run = fanout2();
        let m = run.manifest_for(0);
        assert_eq!(m.agent_id.as_str(), "wf-node0");
        // fanout workers are Explore → ReadOnly + SystemOnly (workflow role_defaults)
        assert_eq!(m.isolation, crate::types::agent::AgentIsolation::ReadOnly);
        assert_eq!(
            m.context_inheritance,
            crate::types::agent::ContextInheritance::SystemOnly
        );
    }

    #[test]
    fn unknown_agent_completion_is_none() {
        let mut run = fanout2();
        assert_eq!(run.record_completion("not-a-node", done()), None);
    }

    #[test]
    fn spawn_info_carries_model_hint_and_trust() {
        use crate::orchestration::workflow::{WorkflowNode, WorkflowSpec};
        use crate::types::agent::AgentRole;

        let spec = WorkflowSpec::new(vec![
            WorkflowNode::new(RuntimeTask::new("read tickets"), AgentRole::Explore)
                .quarantined()
                .with_model_hint("haiku"),
            WorkflowNode::new(RuntimeTask::new("act"), AgentRole::Implement),
        ]);
        let run = WorkflowRun::new(&spec).unwrap();

        // W3: quarantined node + W4: model hint both reach the spawn descriptor.
        let q = run.spawn_info(0);
        assert_eq!(q.trust, "quarantined");
        assert_eq!(q.model_hint.as_deref(), Some("haiku"));
        // default node is trusted, no model hint.
        let t = run.spawn_info(1);
        assert_eq!(t.trust, "trusted");
        assert_eq!(t.model_hint, None);
    }

    #[test]
    fn spawn_info_carries_token_budget() {
        use crate::orchestration::workflow::{WorkflowNode, WorkflowSpec};
        use crate::types::agent::AgentRole;

        let spec = WorkflowSpec::new(vec![
            WorkflowNode::new(RuntimeTask::new("expensive"), AgentRole::Implement)
                .with_token_budget(10_000),
            WorkflowNode::new(RuntimeTask::new("plain"), AgentRole::Implement),
        ]);
        let run = WorkflowRun::new(&spec).unwrap();
        assert_eq!(run.spawn_info(0).token_budget, Some(10_000));
        assert_eq!(run.spawn_info(1).token_budget, None);
    }

    #[test]
    fn spawn_info_carries_dep_ids_and_per_node_caps() {
        // W-N2: EVERY dependent node carries its dependencies' agent ids (a DAG edge carries data);
        // W-N7: per-node max_turns/max_wall_ms ride the same hop chain as token_budget.
        let spec = WorkflowSpec::new(vec![
            WorkflowNode::new(RuntimeTask::new("w"), AgentRole::Explore),
            WorkflowNode::new(RuntimeTask::new("synth"), AgentRole::Plan)
                .with_depends_on(vec![0])
                .with_max_turns(4)
                .with_max_wall_ms(30_000),
        ]);
        let run = WorkflowRun::new(&spec).unwrap();
        let info = run.spawn_info(1);
        assert_eq!(info.input_agent_ids, vec!["wf-node0"]);
        assert_eq!(info.max_turns, Some(4));
        assert_eq!(info.max_wall_ms, Some(30_000));
        let root = run.spawn_info(0);
        assert!(root.input_agent_ids.is_empty());
        assert_eq!(root.max_turns, None);
    }

    use crate::orchestration::workflow::{WorkflowNode, WorkflowSpec};
    use crate::types::agent::AgentRole;

    // ── P3 DAG scheduler scenarios (F1 critical-path / F2 loop fairness / F3 failure propagation) ─
    //
    // These make the deferred orchestration A/B concrete: with `scheduler_policy` now a first-class
    // config axis, scheduling behavior is single-variable testable instead of agent-driven. Each
    // scenario drives the deterministic scheduler and asserts the property the audit called out.

    /// F1 — critical-path skew: among ready siblings, the one heading the longest downstream chain is
    /// scheduled first, overriding node-id order.
    #[test]
    fn f1_critical_path_node_is_scheduled_before_a_lower_id_leaf() {
        use crate::orchestration::workflow::{WorkflowNode, WorkflowSpec};
        use crate::scheduler::policy::SchedulerPolicyConfig;
        use crate::types::agent::AgentRole;

        // node 0 = a leaf (critical path 1). node 1 = root of chain 1→2→3 (critical path 3).
        // Both are ready at the start; the longer critical path must win over the smaller id.
        let spec = WorkflowSpec::new(vec![
            WorkflowNode::new(RuntimeTask::new("leaf"), AgentRole::Implement),
            WorkflowNode::new(RuntimeTask::new("chain-root"), AgentRole::Implement),
            WorkflowNode::new(RuntimeTask::new("mid"), AgentRole::Implement)
                .with_depends_on(vec![1]),
            WorkflowNode::new(RuntimeTask::new("tail"), AgentRole::Implement)
                .with_depends_on(vec![2]),
        ]);
        let mut run = WorkflowRun::new(&spec).unwrap();
        run.set_scheduler_policy(SchedulerPolicyConfig::default());

        assert_eq!(
            run.ready_batch(),
            vec![1, 0],
            "the deeper critical path (node 1) outranks the lower-id leaf (node 0)"
        );
    }

    #[test]
    fn workflow_source_factors_survive_checkpoint_rebuild_and_preserve_ready_order() {
        use crate::orchestration::task_graph::SchedulingFactors;
        use crate::scheduler::policy::SchedulerPolicyConfig;

        let spec = WorkflowSpec::new(vec![
            WorkflowNode::new(RuntimeTask::new("ordinary"), AgentRole::Implement),
            WorkflowNode::new(RuntimeTask::new("urgent"), AgentRole::Implement)
                .with_scheduling_factors(SchedulingFactors {
                    deadline_urgency: 1,
                    process_priority: 0,
                    resource_pressure: 0,
                    budget_pressure: 0,
                }),
        ]);
        let policy = SchedulerPolicyConfig {
            critical_path_weight: 0,
            fanout_weight: 0,
            age_weight: 0,
            token_cost_weight: 0,
            deadline_weight: 1,
            process_priority_weight: 0,
            resource_pressure_weight: 0,
            budget_pressure_weight: 0,
            ..SchedulerPolicyConfig::default()
        };
        let mut original = WorkflowRun::new(&spec).unwrap();
        original.set_scheduler_policy(policy);
        assert_eq!(original.ready_batch(), vec![1, 0]);

        let states = original.checkpoint_nodes();
        let mut restored = WorkflowRun::restore_from_checkpoint(&spec, &states).unwrap();
        restored.set_scheduler_policy(policy);
        assert_eq!(restored.ready_batch(), vec![1, 0]);
    }

    /// F3 — failure propagation: a failure skips its transitive successors, and partial results gate
    /// per the downstream node's dependency policy.
    #[test]
    fn f3_failure_and_partial_propagate_transitively_by_policy() {
        use crate::orchestration::workflow::{DependencyPolicy, WorkflowNode, WorkflowSpec};
        use crate::types::agent::AgentRole;

        // Transitive skip: A(0) fails → B(1) depends on A → C(2) depends on B. Both B and C must
        // close as SkippedUpstreamFailed, not strand in Pending.
        let spec = WorkflowSpec::new(vec![
            WorkflowNode::new(RuntimeTask::new("a"), AgentRole::Implement),
            WorkflowNode::new(RuntimeTask::new("b"), AgentRole::Implement).with_depends_on(vec![0]),
            WorkflowNode::new(RuntimeTask::new("c"), AgentRole::Implement).with_depends_on(vec![1]),
        ]);
        let mut run = WorkflowRun::new(&spec).unwrap();
        run.mark_spawned(0, "wf-node0");
        run.record_completion("wf-node0", terminated(TerminationReason::Error));
        let outcomes = run.finish();
        assert_eq!(outcomes[0].status, WorkflowNodeStatus::Failed);
        assert_eq!(
            outcomes[1].status,
            WorkflowNodeStatus::SkippedUpstreamFailed
        );
        assert_eq!(
            outcomes[2].status,
            WorkflowNodeStatus::SkippedUpstreamFailed,
            "the failure propagates through the whole chain"
        );

        // Partial gates by policy: an upstream partial blocks an AllSuccess dependent but satisfies
        // an AcceptPartial one.
        let spec = WorkflowSpec::new(vec![
            WorkflowNode::new(RuntimeTask::new("up"), AgentRole::Implement),
            WorkflowNode::new(RuntimeTask::new("strict"), AgentRole::Implement)
                .with_depends_on(vec![0]),
            WorkflowNode::new(RuntimeTask::new("lenient"), AgentRole::Implement)
                .with_depends_on(vec![0])
                .with_dependency_policy(DependencyPolicy::AcceptPartial),
        ]);
        let mut run = WorkflowRun::new(&spec).unwrap();
        run.mark_spawned(0, "wf-node0");
        run.record_completion("wf-node0", terminated(TerminationReason::Timeout)); // partial
        assert_eq!(
            run.ready_batch(),
            vec![2],
            "only the AcceptPartial dependent runs"
        );
        assert_eq!(
            run.node_outcomes()[1].status,
            WorkflowNodeStatus::SkippedUpstreamFailed,
            "the AllSuccess dependent is skipped behind the partial upstream"
        );
    }

    // ── P3 (P0-4) generative property: every node lands in exactly one terminal set ─────────────
    //
    // The audit flagged that totality ("∀ node ∈ exactly one terminal state") held structurally but
    // had no generative coverage — the old executor could strand blocked successors in Pending,
    // appearing in neither the completed nor failed audit set. This drives many pseudo-random DAGs
    // with random dependency policies to completion under random per-node terminations (including
    // denials) and asserts `finish()` closes every node into exactly one terminal status. A
    // regression means the workflow audit is no longer closed.

    struct Lcg(u64);
    impl Lcg {
        fn below(&mut self, n: u64) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (self.0 ^ (self.0 >> 33)) % n.max(1)
        }
    }

    #[test]
    fn finish_closes_every_node_into_exactly_one_terminal_state_over_random_dags() {
        use crate::orchestration::workflow::{DependencyPolicy, WorkflowNode, WorkflowSpec};
        use crate::types::agent::AgentRole;
        use std::collections::BTreeSet;

        let terminations = [
            TerminationReason::Completed,
            TerminationReason::MaxTurns,
            TerminationReason::TokenBudget,
            TerminationReason::Timeout,
            TerminationReason::ContextOverflow,
            TerminationReason::NoProgress,
            TerminationReason::MilestoneExceeded,
            TerminationReason::Error,
            TerminationReason::UserAbort,
        ];
        let policies = [
            DependencyPolicy::AllSuccess,
            DependencyPolicy::AcceptPartial,
            DependencyPolicy::AllTerminal,
            DependencyPolicy::Optional,
        ];

        for seed in 0..300u64 {
            let mut rng = Lcg(seed.wrapping_mul(0x9E3779B97F4A7C15).wrapping_add(1));
            let n = 2 + rng.below(7) as usize;

            // Random DAG: node i depends on a random subset of earlier nodes (backward edges only,
            // so it is always acyclic), with a random dependency policy.
            let mut nodes = Vec::new();
            for i in 0..n {
                let mut deps = Vec::new();
                for j in 0..i {
                    if rng.below(3) == 0 {
                        deps.push(j);
                    }
                }
                let policy = policies[rng.below(policies.len() as u64) as usize];
                nodes.push(
                    WorkflowNode::new(RuntimeTask::new(format!("n{i}")), AgentRole::Implement)
                        .with_depends_on(deps)
                        .with_dependency_policy(policy),
                );
            }
            let spec = WorkflowSpec::new(nodes);
            let mut run = WorkflowRun::new(&spec).unwrap();

            // Drive to a fixpoint: spawn each ready node, then either complete it with a random
            // termination or deny it. Bounded so a bug cannot hang the test.
            for _ in 0..(n * 4 + 4) {
                let ready = run.ready_batch();
                if ready.is_empty() {
                    break;
                }
                for node in ready {
                    let agent = node_agent_id(node);
                    run.mark_spawned(node, &agent);
                    if rng.below(5) == 0 {
                        run.mark_denied(node);
                    } else {
                        let termination =
                            terminations[rng.below(terminations.len() as u64) as usize];
                        run.record_completion(&agent, terminated(termination));
                    }
                }
            }

            let outcomes = run.finish();
            // Totality: exactly one terminal outcome per node, covering the whole node set.
            assert_eq!(outcomes.len(), n, "seed {seed}: every node has an outcome");
            let ids: BTreeSet<String> = outcomes.iter().map(|o| o.node_id.clone()).collect();
            assert_eq!(ids.len(), n, "seed {seed}: node ids are unique");
            for node in 0..n {
                assert!(
                    ids.contains(&node_agent_id(node)),
                    "seed {seed}: node {node} is in the closed outcome set"
                );
            }
            for outcome in &outcomes {
                assert!(
                    matches!(
                        outcome.status,
                        WorkflowNodeStatus::Completed
                            | WorkflowNodeStatus::CompletedPartial
                            | WorkflowNodeStatus::Failed
                            | WorkflowNodeStatus::SkippedUpstreamFailed
                    ),
                    "seed {seed}: {} is terminal",
                    outcome.node_id
                );
            }
        }
    }
}
