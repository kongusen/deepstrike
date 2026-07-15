use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashSet};

use crate::scheduler::policy::SchedulerPolicyConfig;
use crate::types::error::{DeepStrikeError, Result};
use crate::types::result::LoopResult;
use crate::types::task::RuntimeTask;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskStatus {
    Pending,
    Ready,
    Running,
    Completed,
    CompletedPartial,
    Failed,
    SkippedUpstreamFailed,
}

impl TaskStatus {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::CompletedPartial | Self::Failed | Self::SkippedUpstreamFailed
        )
    }
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
/// Maintains persistent reverse adjacency and a deterministic ready heap. Completing a node visits
/// only its outgoing dependents; selecting ready work never scans the graph.
pub struct TaskGraph {
    nodes: Vec<TaskNode>,
    /// Number of dependencies that have not completed successfully per task. Workflow-level
    /// policies handle partial/failure terminal states explicitly.
    in_degree: Vec<usize>,
    /// Persistent dependency → dependents index. Terminal promotion touches only outgoing edges.
    reverse_adjacency: Vec<Vec<usize>>,
    ready_heap: BinaryHeap<ReadyEntry>,
    ready_generation: Vec<u64>,
    enqueued_round: Vec<u64>,
    enqueue_sequence: u64,
    ready_round: u64,
    scheduling: Vec<SchedulingMetadata>,
    scheduler_policy: SchedulerPolicyConfig,
}

#[derive(Debug, Clone, Copy, Default)]
struct SchedulingMetadata {
    critical_path_remaining: u64,
    downstream_fanout: u64,
    token_cost: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ReadyEntry {
    priority: i128,
    enqueue_sequence: u64,
    node_id: usize,
    generation: u64,
}

impl Ord for ReadyEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        self.priority
            .cmp(&other.priority)
            .then_with(|| other.enqueue_sequence.cmp(&self.enqueue_sequence))
            .then_with(|| other.node_id.cmp(&self.node_id))
    }
}

impl PartialOrd for ReadyEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl TaskGraph {
    pub fn new() -> Self {
        Self {
            nodes: Vec::new(),
            in_degree: Vec::new(),
            reverse_adjacency: Vec::new(),
            ready_heap: BinaryHeap::new(),
            ready_generation: Vec::new(),
            enqueued_round: Vec::new(),
            enqueue_sequence: 0,
            ready_round: 0,
            scheduling: Vec::new(),
            scheduler_policy: SchedulerPolicyConfig::default(),
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
        let max_index = dependencies.iter().copied().max().unwrap_or(id).max(id);
        self.reverse_adjacency.resize_with(max_index + 1, Vec::new);
        for &dependency in &dependencies {
            self.reverse_adjacency[dependency].push(id);
        }
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
        self.ready_generation.push(0);
        self.enqueued_round.push(self.ready_round);
        self.scheduling.push(SchedulingMetadata::default());
        if deg == 0 {
            self.enqueue_ready(id);
        }
        id
    }

    /// Topological sort — returns ordered IDs or error if cycle detected.
    pub fn topological_sort(&self) -> Result<Vec<usize>> {
        let n = self.nodes.len();
        // `self.in_degree` is the live residual count and is mutated as tasks complete. A
        // topological validation must always start from the immutable graph shape, otherwise
        // validating a resumed/partially completed graph double-decrements edges and underflows.
        let mut in_deg: Vec<usize> = self
            .nodes
            .iter()
            .map(|node| node.dependencies.len())
            .collect();

        let mut queue: Vec<usize> = (0..n).filter(|&i| in_deg[i] == 0).collect();
        let mut order = Vec::with_capacity(n);

        while let Some(id) = queue.pop() {
            order.push(id);
            for &next in self.reverse_adjacency.get(id).into_iter().flatten() {
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
    pub fn ready_tasks(&mut self) -> Vec<usize> {
        // Drain the live heap so stale generations from loop re-arms are discarded instead of
        // accumulating for the lifetime of a long workflow. Valid entries are reinserted because
        // the caller may start only a concurrency-limited prefix of this ordered snapshot.
        let mut valid_entries = Vec::new();
        let mut ready = Vec::new();
        while let Some(entry) = self.ready_heap.pop() {
            if self.nodes.get(entry.node_id).map(|node| node.status) == Some(TaskStatus::Ready)
                && self.ready_generation[entry.node_id] == entry.generation
            {
                ready.push(entry.node_id);
                valid_entries.push(entry);
            }
        }
        self.ready_heap.extend(valid_entries);
        self.ready_round = self.ready_round.saturating_add(1);
        ready
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
            if node.status != TaskStatus::Ready {
                node.status = TaskStatus::Ready;
                self.enqueue_ready(task_id);
            }
        }
    }

    /// Mark a task as completed; promote dependents whose in-degree reaches 0.
    ///
    /// Idempotent: a task already terminal (Completed/Failed) is left untouched — a duplicate
    /// completion (at-least-once event delivery, resume replay) must not double-decrement its
    /// dependents' in-degree, which would underflow (debug panic) or over-promote gated nodes.
    pub fn complete(&mut self, task_id: usize, result: LoopResult) {
        {
            let Some(node) = self.nodes.get_mut(task_id) else {
                return;
            };
            if node.status.is_terminal() {
                return;
            }
            node.status = TaskStatus::Completed;
            node.result = Some(result);
        }
        let dependents = self
            .reverse_adjacency
            .get(task_id)
            .cloned()
            .unwrap_or_default();
        for dep_id in dependents {
            self.in_degree[dep_id] -= 1;
            if self.in_degree[dep_id] == 0 {
                let should_enqueue =
                    self.nodes.get(dep_id).map(|n| n.status) == Some(TaskStatus::Pending);
                if should_enqueue {
                    self.nodes[dep_id].status = TaskStatus::Ready;
                    self.enqueue_ready(dep_id);
                }
            }
        }
    }

    pub fn complete_partial(&mut self, task_id: usize, result: LoopResult) {
        if let Some(node) = self.nodes.get_mut(task_id) {
            if !node.status.is_terminal() {
                node.status = TaskStatus::CompletedPartial;
                node.result = Some(result);
            }
        }
    }

    /// Mark a task as failed (dependents remain Pending — caller decides policy). Terminal states
    /// are sticky: failing an already-completed task must not un-complete it (idempotency twin of
    /// [`complete`](Self::complete)).
    pub fn fail(&mut self, task_id: usize) {
        if let Some(node) = self.nodes.get_mut(task_id) {
            if !node.status.is_terminal() {
                node.status = TaskStatus::Failed;
            }
        }
    }

    pub fn fail_with_result(&mut self, task_id: usize, result: LoopResult) {
        if let Some(node) = self.nodes.get_mut(task_id) {
            if !node.status.is_terminal() {
                node.status = TaskStatus::Failed;
                node.result = Some(result);
            }
        }
    }

    pub fn skip_upstream_failed(&mut self, task_id: usize) {
        if let Some(node) = self.nodes.get_mut(task_id) {
            if !node.status.is_terminal() {
                node.status = TaskStatus::SkippedUpstreamFailed;
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
        self.nodes.iter().all(|n| n.status.is_terminal())
    }

    pub fn configure_scheduling(&mut self, policy: SchedulerPolicyConfig, token_costs: &[u64]) {
        self.scheduler_policy = policy;
        let order = self
            .topological_sort()
            .unwrap_or_else(|_| (0..self.nodes.len()).collect());
        let mut reachable: Vec<HashSet<usize>> = vec![HashSet::new(); self.nodes.len()];
        for &node in order.iter().rev() {
            let mut critical = 1u64;
            let children = self
                .reverse_adjacency
                .get(node)
                .cloned()
                .unwrap_or_default();
            for child in children {
                critical = critical.max(1 + self.scheduling[child].critical_path_remaining);
                reachable[node].insert(child);
                let descendants: Vec<usize> = reachable[child].iter().copied().collect();
                reachable[node].extend(descendants);
            }
            self.scheduling[node] = SchedulingMetadata {
                critical_path_remaining: critical,
                downstream_fanout: reachable[node].len() as u64,
                token_cost: token_costs.get(node).copied().unwrap_or(0),
            };
        }
        self.rebuild_ready_heap();
    }

    fn rebuild_ready_heap(&mut self) {
        self.ready_heap.clear();
        for node_id in 0..self.nodes.len() {
            if self.nodes[node_id].status == TaskStatus::Ready {
                self.push_ready_entry(node_id);
            }
        }
    }

    fn enqueue_ready(&mut self, task_id: usize) {
        self.ready_generation[task_id] = self.ready_generation[task_id].saturating_add(1);
        self.enqueued_round[task_id] = self.ready_round;
        self.enqueue_sequence = self.enqueue_sequence.saturating_add(1);
        self.push_ready_entry(task_id);
    }

    fn push_ready_entry(&mut self, task_id: usize) {
        let metadata = self.scheduling[task_id];
        let policy = self.scheduler_policy;
        let priority = i128::from(policy.critical_path_weight)
            * i128::from(metadata.critical_path_remaining)
            + i128::from(policy.fanout_weight) * i128::from(metadata.downstream_fanout)
            - i128::from(policy.age_weight) * i128::from(self.enqueued_round[task_id])
            - i128::from(policy.token_cost_weight) * i128::from(metadata.token_cost);
        self.ready_heap.push(ReadyEntry {
            priority,
            enqueue_sequence: self.enqueue_sequence,
            node_id: task_id,
            generation: self.ready_generation[task_id],
        });
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

    #[test]
    fn critical_path_priority_beats_lower_node_id() {
        let mut g = TaskGraph::new();
        let wide = g.add(RuntimeTask::new("wide"), vec![]);
        let chain = g.add(RuntimeTask::new("chain"), vec![]);
        g.add(RuntimeTask::new("wide-child-a"), vec![wide]);
        g.add(RuntimeTask::new("wide-child-b"), vec![wide]);
        let chain_2 = g.add(RuntimeTask::new("chain-2"), vec![chain]);
        let chain_3 = g.add(RuntimeTask::new("chain-3"), vec![chain_2]);
        g.add(RuntimeTask::new("chain-4"), vec![chain_3]);

        g.configure_scheduling(SchedulerPolicyConfig::default(), &[]);

        assert_eq!(g.ready_tasks(), vec![chain, wide]);
    }

    #[test]
    fn zero_weights_use_fifo_and_loop_rearm_yields() {
        let mut g = TaskGraph::new();
        let loop_node = g.add(RuntimeTask::new("loop"), vec![]);
        let peer = g.add(RuntimeTask::new("peer"), vec![]);
        let policy = SchedulerPolicyConfig {
            critical_path_weight: 0,
            fanout_weight: 0,
            age_weight: 0,
            token_cost_weight: 0,
            ..SchedulerPolicyConfig::default()
        };
        g.configure_scheduling(policy, &[]);
        assert_eq!(g.ready_tasks(), vec![loop_node, peer]);

        g.start(loop_node);
        g.set_ready(loop_node);
        assert_eq!(g.ready_tasks(), vec![peer, loop_node]);
        assert_eq!(
            g.ready_heap.len(),
            2,
            "stale loop generations must be collected"
        );
    }

    #[test]
    fn reverse_adjacency_tracks_only_outgoing_dependents() {
        let mut g = TaskGraph::new();
        let root = g.add(RuntimeTask::new("root"), vec![]);
        let unrelated = g.add(RuntimeTask::new("unrelated"), vec![]);
        let child = g.add(RuntimeTask::new("child"), vec![root]);
        g.add(RuntimeTask::new("grandchild"), vec![child]);

        assert_eq!(g.reverse_adjacency[root], vec![child]);
        assert!(g.reverse_adjacency[unrelated].is_empty());
    }
}
