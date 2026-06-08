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

use crate::orchestration::executor;
use crate::orchestration::task_graph::TaskGraph;
use crate::orchestration::workflow::{WorkflowNode, WorkflowSpec};
use crate::types::agent::IsolationManifest;
use crate::types::error::Result;
use crate::types::result::LoopResult;

/// Deterministic kernel agent id for a workflow node (stable across resume / audit).
pub fn node_agent_id(node: usize) -> String {
    format!("wf-node{node}")
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

    /// True once the current batch has drained (every spawned node reported back).
    pub fn batch_drained(&self) -> bool {
        self.batch.is_empty()
    }

    /// True once every node is terminal (completed or failed) and nothing is in flight.
    pub fn is_complete(&self) -> bool {
        self.graph.all_done() && self.batch.is_empty()
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
}
