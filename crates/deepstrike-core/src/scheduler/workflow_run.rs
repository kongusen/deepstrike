//! W0: a kernel-resident workflow run — the DAG state for one in-flight [`WorkflowSpec`].
//!
//! Pure data + pure advance logic, no I/O and no syscall: the [`crate::scheduler::state_machine::
//! LoopStateMachine`] drives this, gating each ready node's spawn through
//! `evaluate_syscall(Syscall::Spawn)` and reusing the existing batch-await barrier
//! (`SuspendState::SubAgentAwait`). This module only tracks *which* nodes are ready, spawned,
//! done, or denied, and builds each node's [`IsolationManifest`].
//!
//! Lifecycle: `ready_batch()` → (gate each) `mark_spawned` / `mark_denied` → on completion
//! `record_completion` → repeat until `is_complete()`.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::orchestration::executor;
use crate::orchestration::task_graph::{TaskGraph, TaskStatus};
use crate::orchestration::workflow::{NodeTrust, WorkflowNode, WorkflowSpec};
use crate::types::agent::{AgentIsolation, AgentRole, ContextInheritance, IsolationManifest};
use crate::types::error::Result;
use crate::types::result::LoopResult;

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
}

fn default_trust() -> String {
    "trusted".to_string()
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

/// Synthetic terminal result for a node recovered as already-completed during resume.
fn resumed_result() -> LoopResult {
    LoopResult {
        termination: crate::types::result::TerminationReason::Completed,
        final_message: None,
        turns_used: 0,
        total_tokens_used: 0,
    }
}

/// The state of one in-flight workflow execution.
pub struct WorkflowRun {
    graph: TaskGraph,
    nodes: Vec<WorkflowNode>,
    /// Parent session id stamped onto each node's spawned-agent manifest.
    parent_session_id: String,
    /// Completed-event lookup: kernel agent id → DAG node index.
    node_of_agent: HashMap<String, usize>,
    /// Nodes spawned in the current batch, awaiting completion.
    batch: Vec<usize>,
}

impl WorkflowRun {
    /// Build from a spec. Validates dependency indices + acyclicity (reuses `WorkflowSpec`).
    pub fn new(spec: &WorkflowSpec, parent_session_id: &str) -> Result<Self> {
        spec.validate()?;
        Ok(Self {
            graph: spec.to_task_graph()?,
            nodes: spec.nodes.clone(),
            parent_session_id: parent_session_id.to_string(),
            node_of_agent: HashMap::new(),
            batch: Vec::new(),
        })
    }

    /// W0-ABI resume: rebuild an in-flight run by replaying which node agent-ids already completed
    /// (e.g. recovered from the session log after an interruption). Those nodes are pre-marked
    /// done so [`ready_batch`](Self::ready_batch) returns only the remaining work — the kernel then
    /// continues the DAG from where it left off. Unknown ids are ignored.
    pub fn resume(spec: &WorkflowSpec, parent_session_id: &str, completed: &[String]) -> Result<Self> {
        let mut run = Self::new(spec, parent_session_id)?;
        let n = run.graph.len();
        for id in completed {
            if let Some(node) = (0..n).find(|&i| node_agent_id(i) == *id) {
                run.graph.start(node);
                run.graph.complete(node, resumed_result());
            }
        }
        Ok(run)
    }

    /// Node indices whose dependencies are satisfied and that have not yet started.
    pub fn ready_batch(&self) -> Vec<usize> {
        executor::next_batch(&self.graph).runnable
    }

    /// Build the isolation manifest for a node, preserving its explicit isolation +
    /// context-inheritance (the `AgentRunSpec`→`from_spec` path would overwrite these with
    /// role defaults). Capability inheritance for workflow nodes is left to a later round.
    pub fn manifest_for(&self, node: usize) -> IsolationManifest {
        let n = &self.nodes[node];
        IsolationManifest {
            agent_id: node_agent_id(node).into(),
            parent_session_id: self.parent_session_id.as_str().into(),
            role: n.role,
            isolation: n.isolation,
            context_inheritance: n.context_inheritance,
            permitted_capability_ids: Vec::new(),
        }
    }

    /// The goal text for a node (for the spawn's run spec / context injection).
    pub fn goal_of(&self, node: usize) -> &str {
        &self.nodes[node].task.goal
    }

    /// The SDK-facing spawn descriptor for a node (agent id + goal + canonical role/isolation/
    /// inheritance strings + model hint). The kernel owns the spec; this is how the goal reaches
    /// the host that runs the node.
    pub fn spawn_info(&self, node: usize) -> WorkflowSpawnInfo {
        let n = &self.nodes[node];
        WorkflowSpawnInfo {
            agent_id: node_agent_id(node),
            goal: n.task.goal.clone(),
            role: role_label(n.role).to_string(),
            isolation: isolation_label(n.isolation).to_string(),
            context_inheritance: inheritance_label(n.context_inheritance).to_string(),
            model_hint: n.model_hint.clone(),
            trust: trust_label(n.trust).to_string(),
        }
    }

    /// Mark a node as spawned: start it in the graph, record it in the live batch, and map its
    /// kernel agent id back to the node for completion routing.
    pub fn mark_spawned(&mut self, node: usize, agent_id: &str) {
        self.graph.start(node);
        self.batch.push(node);
        self.node_of_agent.insert(agent_id.to_string(), node);
    }

    /// Mark a node as denied by the syscall gate: fail it in the graph (dependents stay pending
    /// and will never become ready). Does not enter the live batch.
    pub fn mark_denied(&mut self, node: usize) {
        self.graph.fail(node);
    }

    /// Record a completed sub-agent against its node. Returns the node index if `agent_id`
    /// belonged to this workflow (and removes it from the live batch), else `None`.
    pub fn record_completion(&mut self, agent_id: &str, result: LoopResult) -> Option<usize> {
        let node = *self.node_of_agent.get(agent_id)?;
        self.graph.complete(node, result);
        self.batch.retain(|&n| n != node);
        Some(node)
    }

    /// Whether `agent_id` belongs to this workflow.
    pub fn owns_agent(&self, agent_id: &str) -> bool {
        self.node_of_agent.contains_key(agent_id)
    }

    /// The parent session id for this workflow (stamped on each node's manifest).
    pub fn parent_session_id(&self) -> &str {
        &self.parent_session_id
    }

    /// True once the current batch has drained (every spawned node reported back).
    pub fn batch_drained(&self) -> bool {
        self.batch.is_empty()
    }

    /// True once every node is terminal (completed or failed) and nothing is in flight.
    pub fn is_complete(&self) -> bool {
        self.graph.all_done() && self.batch.is_empty()
    }

    /// Outcome at finish: `(completed_agent_ids, failed_agent_ids)` by node. Nodes left
    /// `Pending`/`Ready` (stalled behind a gated dependency) appear in neither.
    pub fn outcome(&self) -> (Vec<String>, Vec<String>) {
        let mut completed = Vec::new();
        let mut failed = Vec::new();
        for i in 0..self.graph.len() {
            match self.graph.get(i).map(|n| n.status) {
                Some(TaskStatus::Completed) => completed.push(node_agent_id(i)),
                Some(TaskStatus::Failed) => failed.push(node_agent_id(i)),
                _ => {}
            }
        }
        (completed, failed)
    }

    /// Total node count.
    pub fn len(&self) -> usize {
        self.graph.len()
    }

    pub fn is_empty(&self) -> bool {
        self.graph.is_empty()
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
        }
    }

    fn fanout2() -> WorkflowRun {
        // 2 workers (nodes 0,1) → synthesize (node 2, depends on both)
        let spec = fanout_synthesize(
            vec![RuntimeTask::new("w0"), RuntimeTask::new("w1")],
            RuntimeTask::new("synth"),
        );
        WorkflowRun::new(&spec, "parent-sess").unwrap()
    }

    #[test]
    fn first_batch_is_the_workers() {
        let run = fanout2();
        assert_eq!(run.ready_batch(), vec![0, 1]);
        assert_eq!(run.len(), 3);
        assert!(!run.is_complete());
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
    fn denied_node_blocks_dependents_and_stalls_progress() {
        let mut run = fanout2();
        // node 0 spawned + completes; node 1 denied by the gate
        run.mark_spawned(0, &node_agent_id(0));
        run.mark_denied(1);
        run.record_completion(&node_agent_id(0), done());
        // synth depends on node 1 (failed) → never ready; batch drained, nothing more to run.
        // The state machine finishes a workflow on "drained && ready_batch empty" (here true),
        // even though `is_complete()` is false (node 2 stays Pending forever).
        assert!(run.batch_drained());
        assert!(run.ready_batch().is_empty());
        assert!(!run.is_complete());
    }

    #[test]
    fn manifest_preserves_node_isolation_and_inheritance() {
        let run = fanout2();
        let m = run.manifest_for(0);
        assert_eq!(m.agent_id.as_str(), "wf-node0");
        assert_eq!(m.parent_session_id.as_str(), "parent-sess");
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
    fn resume_skips_already_completed_nodes() {
        // fanout2: workers 0,1 → synth 2. Resume with worker 0 already done.
        let spec = fanout_synthesize(
            vec![RuntimeTask::new("w0"), RuntimeTask::new("w1")],
            RuntimeTask::new("synth"),
        );
        let run = WorkflowRun::resume(&spec, "sess", &[node_agent_id(0)]).unwrap();
        // only the remaining worker (node 1) is ready; node 0 is already complete, synth still gated.
        assert_eq!(run.ready_batch(), vec![1]);
        assert!(!run.is_complete());
    }

    #[test]
    fn resume_with_all_done_completes() {
        let spec = fanout_synthesize(vec![RuntimeTask::new("w0")], RuntimeTask::new("synth"));
        // both nodes (worker 0, synth 1) recovered as done.
        let run = WorkflowRun::resume(&spec, "sess", &[node_agent_id(0), node_agent_id(1)]).unwrap();
        assert!(run.ready_batch().is_empty());
        assert!(run.is_complete());
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
        let run = WorkflowRun::new(&spec, "sess").unwrap();

        // W3: quarantined node + W4: model hint both reach the spawn descriptor.
        let q = run.spawn_info(0);
        assert_eq!(q.trust, "quarantined");
        assert_eq!(q.model_hint.as_deref(), Some("haiku"));
        // default node is trusted, no model hint.
        let t = run.spawn_info(1);
        assert_eq!(t.trust, "trusted");
        assert_eq!(t.model_hint, None);
    }
}
