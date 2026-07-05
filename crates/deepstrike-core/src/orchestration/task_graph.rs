use crate::types::error::{DeepStrikeError, Result};
use crate::types::result::LoopResult;
use crate::types::task::RuntimeTask;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskStatus {
    Pending,
    Ready,
    Running,
    Completed,
    Failed,
}

#[derive(Debug, Clone)]
pub struct TaskNode {
    pub id: usize,
    pub task: RuntimeTask,
    pub status: TaskStatus,
    pub result: Option<LoopResult>,
    pub dependencies: Vec<usize>,
}

/// DAG of tasks with dependency tracking.
/// Maintains an in-degree counter so `ready_tasks()` is O(1) amortized.
pub struct TaskGraph {
    nodes: Vec<TaskNode>,
    /// Number of incomplete dependencies per task.
    in_degree: Vec<usize>,
}

impl TaskGraph {
    pub fn new() -> Self {
        Self {
            nodes: Vec::new(),
            in_degree: Vec::new(),
        }
    }

    /// Add a task, returns its ID. Duplicate dependency entries are collapsed: `in_degree` counts
    /// entries but [`complete`](Self::complete) decrements once per completed dependency, so a
    /// duplicated entry would leave the node permanently below its own in-degree (a silent stall).
    pub fn add(&mut self, task: RuntimeTask, mut dependencies: Vec<usize>) -> usize {
        let mut seen = std::collections::HashSet::new();
        dependencies.retain(|d| seen.insert(*d));
        let id = self.nodes.len();
        let deg = dependencies.len();
        self.nodes.push(TaskNode {
            id,
            task,
            status: if deg == 0 {
                TaskStatus::Ready
            } else {
                TaskStatus::Pending
            },
            result: None,
            dependencies,
        });
        self.in_degree.push(deg);
        id
    }

    /// Topological sort — returns ordered IDs or error if cycle detected.
    pub fn topological_sort(&self) -> Result<Vec<usize>> {
        let n = self.nodes.len();
        let mut in_deg = self.in_degree.clone();
        let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];

        for node in &self.nodes {
            for &dep in &node.dependencies {
                adj[dep].push(node.id);
            }
        }

        let mut queue: Vec<usize> = (0..n).filter(|&i| in_deg[i] == 0).collect();
        let mut order = Vec::with_capacity(n);

        while let Some(id) = queue.pop() {
            order.push(id);
            for &next in &adj[id] {
                in_deg[next] -= 1;
                if in_deg[next] == 0 {
                    queue.push(next);
                }
            }
        }

        if order.len() != n {
            return Err(DeepStrikeError::OrchestrationCycle);
        }
        Ok(order)
    }

    /// Return IDs of tasks that are Ready (deps satisfied, not yet started).
    pub fn ready_tasks(&self) -> Vec<usize> {
        self.nodes
            .iter()
            .filter(|n| n.status == TaskStatus::Ready)
            .map(|n| n.id)
            .collect()
    }

    /// Mark a task as running.
    pub fn start(&mut self, task_id: usize) {
        if let Some(node) = self.nodes.get_mut(task_id) {
            node.status = TaskStatus::Running;
        }
    }

    /// Re-mark a (running) task as Ready without touching dependents — used to re-arm a loop node
    /// for its next iteration. Unlike [`complete`](Self::complete), this does NOT decrement any
    /// in-degree, so the loop node's dependents stay pending until the loop finally `complete`s.
    pub fn set_ready(&mut self, task_id: usize) {
        if let Some(node) = self.nodes.get_mut(task_id) {
            node.status = TaskStatus::Ready;
        }
    }

    /// Mark a task as completed; promote dependents whose in-degree reaches 0.
    ///
    /// Idempotent: a task already terminal (Completed/Failed) is left untouched — a duplicate
    /// completion (at-least-once event delivery, resume replay) must not double-decrement its
    /// dependents' in-degree, which would underflow (debug panic) or over-promote gated nodes.
    pub fn complete(&mut self, task_id: usize, result: LoopResult) {
        let Some(node) = self.nodes.get_mut(task_id) else {
            return;
        };
        if matches!(node.status, TaskStatus::Completed | TaskStatus::Failed) {
            return;
        }
        node.status = TaskStatus::Completed;
        node.result = Some(result);
        // Collect dependents first to avoid borrow conflict
        let dependents: Vec<usize> = self
            .nodes
            .iter()
            .filter(|n| n.dependencies.contains(&task_id))
            .map(|n| n.id)
            .collect();
        for dep_id in dependents {
            self.in_degree[dep_id] -= 1;
            if self.in_degree[dep_id] == 0 {
                if let Some(n) = self.nodes.get_mut(dep_id) {
                    if n.status == TaskStatus::Pending {
                        n.status = TaskStatus::Ready;
                    }
                }
            }
        }
    }

    /// Mark a task as failed (dependents remain Pending — caller decides policy). Terminal states
    /// are sticky: failing an already-completed task must not un-complete it (idempotency twin of
    /// [`complete`](Self::complete)).
    pub fn fail(&mut self, task_id: usize) {
        if let Some(node) = self.nodes.get_mut(task_id) {
            if !matches!(node.status, TaskStatus::Completed | TaskStatus::Failed) {
                node.status = TaskStatus::Failed;
            }
        }
    }

    pub fn get(&self, task_id: usize) -> Option<&TaskNode> {
        self.nodes.get(task_id)
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn all_done(&self) -> bool {
        self.nodes
            .iter()
            .all(|n| matches!(n.status, TaskStatus::Completed | TaskStatus::Failed))
    }
}

impl Default for TaskGraph {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topological_sort_linear() {
        let mut g = TaskGraph::new();
        let a = g.add(RuntimeTask::new("A"), vec![]);
        let b = g.add(RuntimeTask::new("B"), vec![a]);
        let c = g.add(RuntimeTask::new("C"), vec![b]);

        let order = g.topological_sort().unwrap();
        assert_eq!(order, vec![0, 1, 2]);
        let _ = (a, c);
    }

    #[test]
    fn detects_cycle() {
        let mut g = TaskGraph::new();
        g.nodes.push(TaskNode {
            id: 0,
            task: RuntimeTask::new("A"),
            status: TaskStatus::Pending,
            result: None,
            dependencies: vec![1],
        });
        g.nodes.push(TaskNode {
            id: 1,
            task: RuntimeTask::new("B"),
            status: TaskStatus::Pending,
            result: None,
            dependencies: vec![0],
        });
        g.in_degree.push(1);
        g.in_degree.push(1);

        assert!(g.topological_sort().is_err());
    }

    #[test]
    fn ready_tasks_respects_deps() {
        let mut g = TaskGraph::new();
        let a = g.add(RuntimeTask::new("A"), vec![]);
        let _b = g.add(RuntimeTask::new("B"), vec![a]);

        assert_eq!(g.ready_tasks(), vec![0]); // only A is Ready
    }

    #[test]
    fn set_ready_rearms_without_promoting_dependents() {
        let mut g = TaskGraph::new();
        let a = g.add(RuntimeTask::new("A"), vec![]); // loop node
        let b = g.add(RuntimeTask::new("B"), vec![a]); // dependent
        g.start(a);
        // Re-arm A for its next iteration: A is Ready again, but B stays Pending (no promotion).
        g.set_ready(a);
        assert_eq!(g.nodes[a].status, TaskStatus::Ready);
        assert_eq!(g.nodes[b].status, TaskStatus::Pending);
        assert_eq!(g.ready_tasks(), vec![a]);
    }

    #[test]
    fn complete_promotes_dependent() {
        use crate::types::result::{LoopResult, TerminationReason};
        let mut g = TaskGraph::new();
        let a = g.add(RuntimeTask::new("A"), vec![]);
        let b = g.add(RuntimeTask::new("B"), vec![a]);

        assert_eq!(g.nodes[b].status, TaskStatus::Pending);
        g.complete(
            a,
            LoopResult {
                termination: TerminationReason::Completed,
                final_message: None,
                turns_used: 1,
                total_tokens_used: 0,
                loop_continue: None,
                classify_branch: None,
                tournament_winner: None,
                pace_decision: None,
            },
        );
        assert_eq!(g.nodes[b].status, TaskStatus::Ready);
    }

    #[test]
    fn duplicate_complete_is_idempotent() {
        use crate::types::result::{LoopResult, TerminationReason};
        let result = || LoopResult {
            termination: TerminationReason::Completed,
            final_message: None,
            turns_used: 1,
            total_tokens_used: 0,
            loop_continue: None,
            classify_branch: None,
            tournament_winner: None,
            pace_decision: None,
        };
        // b gates on BOTH a and c; a duplicate completion of `a` must not stand in for `c`.
        let mut g = TaskGraph::new();
        let a = g.add(RuntimeTask::new("A"), vec![]);
        let c = g.add(RuntimeTask::new("C"), vec![]);
        let b = g.add(RuntimeTask::new("B"), vec![a, c]);

        g.complete(a, result());
        g.complete(a, result()); // duplicate delivery — no double decrement, no panic
        assert_eq!(g.nodes[b].status, TaskStatus::Pending);
        g.complete(c, result());
        assert_eq!(g.nodes[b].status, TaskStatus::Ready);
        // Terminal states are sticky both ways.
        g.fail(a);
        assert_eq!(g.nodes[a].status, TaskStatus::Completed);
    }
}
